//! Palette-colour TIFF: the one still format the `image` crate refuses.
//!
//! A TIFF is a header pointing at an *image file directory* — a table of tagged
//! fields naming the picture's width, depth, compression and where its pixels
//! live — followed by those pixels in strips. When the `PhotometricInterpretation`
//! tag says `3` the pixels are palette indices and a `ColorMap` tag holds the
//! palette, which is the form a converted 8-bit picture arrives in.
//!
//! That form is exactly what the `image` crate hands back an error for: the
//! `tiff` crate under it errors out of `colortype()` for `RGBPalette` before any
//! pixels are read, and `ColorMap` is an unread tag there. So the whole file
//! is parsed here instead — but *only* the palette case. Everything else in a
//! TIFF (truecolour, greyscale, CMYK, tiles, JPEG-in-TIFF) is left to the
//! `image` crate, which decodes it and decodes it better; this decoder bails
//! and [`ImageEmu`](crate::image_emu::ImageEmu) falls through to it.
//!
//! Decoding into the same [`IndexedImage`] the ILBM and DEGAS decoders produce
//! is the point of doing it here rather than expanding to RGBA: the palette
//! survives, so a paletted TIFF is a real paletted picture in the frontend like
//! an `.iff` or a `.pi1` and not a truecolour still. TIFF has no equivalent of a
//! CRNG chunk, so nothing cycles by itself, but the representation matches.
//!
//! Supported: 1/2/4/8 bits per pixel, either byte order, striped images with
//! no compression, PackBits or LZW, and horizontal differencing. That covers
//! what picture converters actually write; anything else is an error naming
//! what it ran into.

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail, ensure};

use crate::ilbm::{IndexedImage, unpack_byterun1};

/// The tags this decoder reads. TIFF's field numbers are the format's whole
/// vocabulary — a decoder knows a file only by the tags it recognises.
const TAG_IMAGE_WIDTH: u16 = 256;
const TAG_IMAGE_LENGTH: u16 = 257;
const TAG_BITS_PER_SAMPLE: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_FILL_ORDER: u16 = 266;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_SAMPLES_PER_PIXEL: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_BYTE_COUNTS: u16 = 279;
const TAG_PREDICTOR: u16 = 317;
const TAG_COLOR_MAP: u16 = 320;
const TAG_TILE_WIDTH: u16 = 322;

/// The `PhotometricInterpretation` value meaning "the samples are indices into
/// `ColorMap`". The only one this module claims.
const PHOTOMETRIC_RGB_PALETTE: u32 = 3;

/// The `Compression` values a paletted TIFF is written with in practice.
/// PackBits is the same run-length coding as an IFF BODY's ByteRun1, so
/// [`unpack_byterun1`] decodes it unchanged.
const COMPRESSION_NONE: u32 = 1;
const COMPRESSION_LZW: u32 = 5;
const COMPRESSION_PACKBITS: u32 = 32773;

/// `Predictor` values: pixels stored as they are, or each one stored as its
/// difference from the pixel to its left (which packs flat gradients better).
const PREDICTOR_NONE: u32 = 1;
const PREDICTOR_HORIZONTAL: u32 = 2;

/// Largest picture accepted, in either axis. TIFF states its dimensions in
/// 32-bit fields, so without a bound a corrupt header would have us allocate
/// gigabytes before reading a single strip. Far above any real picture.
const MAX_DIMENSION: u32 = 1 << 16;

/// LZW's two reserved codes and the width limits around them. Codes are packed
/// most-significant bit first and start out 9 bits wide, growing by one each
/// time the string table fills, up to 12.
const LZW_CLEAR: u16 = 256;
const LZW_EOI: u16 = 257;
const LZW_FIRST_CODE: usize = 258;
const LZW_MIN_WIDTH: u32 = 9;
const LZW_MAX_WIDTH: u32 = 12;

/// Which end of a multi-byte field its most significant byte is at. Stated by
/// the first two bytes of the file and true of every field in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Order {
    Little,
    Big,
}

impl Order {
    fn u16(self, bytes: &[u8], at: usize) -> Result<u16> {
        let field = read_field::<2>(bytes, at)?;
        Ok(match self {
            Order::Little => u16::from_le_bytes(field),
            Order::Big => u16::from_be_bytes(field),
        })
    }

    fn u32(self, bytes: &[u8], at: usize) -> Result<u32> {
        let field = read_field::<4>(bytes, at)?;
        Ok(match self {
            Order::Little => u32::from_le_bytes(field),
            Order::Big => u32::from_be_bytes(field),
        })
    }
}

/// `N` bytes at `at`, or an error naming where the file ran out. Every read of
/// the file goes through here, so a truncated or lying file is a message rather
/// than a panic.
fn read_field<const N: usize>(bytes: &[u8], at: usize) -> Result<[u8; N]> {
    bytes
        .get(at..at + N)
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| anyhow!("TIFF ends inside a field at offset {at}"))
}

/// Bytes one value of field type `kind` occupies, or `None` for a type this
/// decoder can't turn into an integer.
fn field_size(kind: u16) -> Option<usize> {
    match kind {
        // BYTE, ASCII, SBYTE, UNDEFINED
        1 | 2 | 6 | 7 => Some(1),
        // SHORT, SSHORT
        3 | 8 => Some(2),
        // LONG, SLONG, FLOAT
        4 | 9 | 11 => Some(4),
        // RATIONAL, SRATIONAL, DOUBLE
        5 | 10 | 12 => Some(8),
        _ => None,
    }
}

/// One directory entry: what type its values are, how many there are, and the
/// offset they start at. TIFF stores values inline in the entry when they fit
/// in its four value bytes and out of line when they don't; `at` is resolved to
/// an absolute file offset either way, so readers need not care which it was.
struct Entry {
    kind: u16,
    count: u32,
    at: usize,
}

/// An image file directory, indexed by tag.
struct Ifd<'a> {
    bytes: &'a [u8],
    order: Order,
    entries: HashMap<u16, Entry>,
}

impl<'a> Ifd<'a> {
    /// Parse the header and the first directory it points at. A multi-page
    /// TIFF's later pages are ignored: a picture file has one.
    fn first(bytes: &'a [u8]) -> Result<Self> {
        let order = match bytes.get(0..2) {
            Some(b"II") => Order::Little,
            Some(b"MM") => Order::Big,
            _ => bail!("not a TIFF: no byte order mark"),
        };
        // 42 is baseline TIFF; 43 is BigTIFF, whose offsets are 64-bit and
        // whose directories are laid out differently enough to need their own
        // reader. No picture converter writes one.
        let magic = order.u16(bytes, 2)?;
        ensure!(magic == 42, "not a baseline TIFF (version {magic})");

        let start = order.u32(bytes, 4)? as usize;
        let count = order.u16(bytes, start)? as usize;
        let mut entries = HashMap::with_capacity(count);
        for i in 0..count {
            // Twelve bytes each: tag, type, value count, then four bytes that
            // are either the values or an offset to them.
            let at = start + 2 + i * 12;
            let tag = order.u16(bytes, at)?;
            let kind = order.u16(bytes, at + 2)?;
            let count = order.u32(bytes, at + 4)?;
            let size = field_size(kind).unwrap_or(0) as u64 * u64::from(count);
            let at = if size > 4 {
                order.u32(bytes, at + 8)? as usize
            } else {
                at + 8
            };
            entries.insert(tag, Entry { kind, count, at });
        }
        Ok(Ifd {
            bytes,
            order,
            entries,
        })
    }

    fn has(&self, tag: u16) -> bool {
        self.entries.contains_key(&tag)
    }

    /// Every value of an integer-typed tag, widened to `u32`.
    fn values(&self, tag: u16) -> Result<Vec<u32>> {
        let entry = self
            .entries
            .get(&tag)
            .ok_or_else(|| anyhow!("TIFF is missing tag {tag}"))?;
        let size = field_size(entry.kind)
            .filter(|size| *size <= 4)
            .ok_or_else(|| anyhow!("TIFF tag {tag} has unreadable field type {}", entry.kind))?;
        (0..entry.count as usize)
            .map(|i| {
                let at = entry.at + i * size;
                match size {
                    1 => Ok(u32::from(read_field::<1>(self.bytes, at)?[0])),
                    2 => Ok(u32::from(self.order.u16(self.bytes, at)?)),
                    _ => self.order.u32(self.bytes, at),
                }
            })
            .collect()
    }

    /// The first value of a tag that carries exactly one.
    fn value(&self, tag: u16) -> Result<u32> {
        self.values(tag)?
            .first()
            .copied()
            .ok_or_else(|| anyhow!("TIFF tag {tag} has no value"))
    }

    /// The first value of a tag, or the default the TIFF specification gives it
    /// when the file leaves the tag out.
    fn value_or(&self, tag: u16, default: u32) -> Result<u32> {
        match self.has(tag) {
            true => self.value(tag),
            false => Ok(default),
        }
    }
}

/// Decode a palette-colour TIFF, keeping the palette and the per-pixel indices
/// so the picture stays paletted all the way to the frontend.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    let ifd = Ifd::first(bytes)?;

    // Anything but a palette belongs to the `image` crate, so say so plainly
    // and let the caller fall through to it.
    let photometric = ifd.value(TAG_PHOTOMETRIC)?;
    ensure!(
        photometric == PHOTOMETRIC_RGB_PALETTE,
        "not a palette-colour TIFF (photometric interpretation {photometric})"
    );
    // An index is a single sample by definition, which also settles
    // `PlanarConfiguration`: with one sample its two values describe the same
    // layout, so the tag is not worth reading.
    let samples = ifd.value_or(TAG_SAMPLES_PER_PIXEL, 1)?;
    ensure!(
        samples == 1,
        "palette TIFF with {samples} samples per pixel"
    );
    let bits = ifd.value_or(TAG_BITS_PER_SAMPLE, 1)?;
    ensure!(
        matches!(bits, 1 | 2 | 4 | 8),
        "palette TIFF with {bits} bits per sample"
    );
    // Reversed fill order puts the first pixel in a byte's low bits. It is
    // legal and essentially unused; refusing it beats decoding it mirrored.
    let fill_order = ifd.value_or(TAG_FILL_ORDER, 1)?;
    ensure!(fill_order == 1, "TIFF with reversed fill order");
    // Tiles replace strips with a grid of rectangles, and bring their own set
    // of tags. Converters write strips.
    ensure!(!ifd.has(TAG_TILE_WIDTH), "tiled TIFF");

    let width = ifd.value(TAG_IMAGE_WIDTH)?;
    let height = ifd.value(TAG_IMAGE_LENGTH)?;
    ensure!(width > 0 && height > 0, "TIFF with an empty image");
    ensure!(
        width <= MAX_DIMENSION && height <= MAX_DIMENSION,
        "TIFF is implausibly large ({width}x{height})"
    );
    let (width, height) = (width as usize, height as usize);

    let data = read_strips(&ifd, bits as usize, width, height)?;
    let indices = unpack_samples(&data, bits as usize, width, height);
    Ok(IndexedImage {
        width: width as u32,
        height: height as u32,
        palette: read_palette(&ifd, bits)?,
        indices,
        // Nothing in a TIFF describes colour cycling.
        ranges: Vec::new(),
    })
}

/// The palette, as RGB triplets — one per value the pixel depth can hold.
///
/// `ColorMap` stores its channels as three consecutive runs rather than
/// interleaved: every red, then every green, then every blue.
fn read_palette(ifd: &Ifd, bits: u32) -> Result<Vec<[u8; 3]>> {
    let colors = 1usize << bits;
    let map = ifd.values(TAG_COLOR_MAP)?;
    ensure!(
        map.len() == colors * 3,
        "TIFF colour map has {} entries, expected {}",
        map.len(),
        colors * 3
    );
    // The channels are specified as full-range 16-bit values, so the top byte
    // is the colour. Some writers put an 8-bit value in the 16-bit field
    // instead, which would decode as an all-black palette; a map that never
    // exceeds 255 is taken at face value, as no real 16-bit palette is that
    // uniformly dark.
    let wide = map.iter().any(|&v| v > 255);
    let channel = |v: u32| if wide { (v >> 8) as u8 } else { v as u8 };
    Ok((0..colors)
        .map(|i| {
            [
                channel(map[i]),
                channel(map[colors + i]),
                channel(map[colors * 2 + i]),
            ]
        })
        .collect())
}

/// Decompress every strip into one buffer of packed rows.
///
/// A strip is `RowsPerStrip` rows of the picture compressed on its own, so the
/// strips concatenated are the whole bitmap. Each row starts on a byte
/// boundary, which is what makes the sub-8-bit depths worth unpacking
/// separately (see [`unpack_samples`]).
fn read_strips(ifd: &Ifd, bits: usize, width: usize, height: usize) -> Result<Vec<u8>> {
    let compression = ifd.value_or(TAG_COMPRESSION, COMPRESSION_NONE)?;
    let predictor = ifd.value_or(TAG_PREDICTOR, PREDICTOR_NONE)?;
    // Left out, the tag means the whole image is one strip. Capping it at the
    // height keeps the arithmetic below in range for that (the tag's default is
    // 2^32 - 1) without changing what it means.
    let rows_per_strip = (ifd.value_or(TAG_ROWS_PER_STRIP, u32::MAX)? as usize).min(height);
    ensure!(rows_per_strip > 0, "TIFF with zero rows per strip");

    let offsets = ifd.values(TAG_STRIP_OFFSETS)?;
    let counts = ifd.values(TAG_STRIP_BYTE_COUNTS)?;
    ensure!(
        offsets.len() == counts.len(),
        "TIFF has {} strips but {} strip lengths",
        offsets.len(),
        counts.len()
    );
    ensure!(
        offsets.len() == height.div_ceil(rows_per_strip),
        "TIFF has {} strips for {height} rows of {rows_per_strip}",
        offsets.len()
    );

    let row_bytes = (width * bits).div_ceil(8);
    let mut data = Vec::with_capacity(row_bytes * height);
    for (i, (&at, &len)) in offsets.iter().zip(&counts).enumerate() {
        let rows = rows_per_strip.min(height - i * rows_per_strip);
        let expected = row_bytes * rows;
        let packed = strip_bytes(ifd.bytes, at as usize, len as usize)?;
        let mut strip = match compression {
            COMPRESSION_NONE => packed.to_vec(),
            COMPRESSION_PACKBITS => unpack_byterun1(packed, expected)?.0,
            COMPRESSION_LZW => lzw_decode(packed, expected)?,
            other => bail!("TIFF compression {other} is not supported"),
        };
        ensure!(
            strip.len() >= expected,
            "TIFF strip {i} decoded to {} bytes, expected {expected}",
            strip.len()
        );
        strip.truncate(expected);
        if predictor == PREDICTOR_HORIZONTAL {
            // Differences are taken between samples, so they only line up with
            // bytes at the one depth where a sample is a byte.
            ensure!(
                bits == 8,
                "horizontal differencing at {bits} bits per pixel"
            );
            for row in strip.chunks_exact_mut(row_bytes) {
                for x in 1..row.len() {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        } else {
            ensure!(
                predictor == PREDICTOR_NONE,
                "TIFF predictor {predictor} is not supported"
            );
        }
        data.extend_from_slice(&strip);
    }
    Ok(data)
}

/// A strip's `len` bytes at `at`, or an error naming the strip that pointed
/// outside the file.
fn strip_bytes(bytes: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    bytes
        .get(at..)
        .and_then(|rest| rest.get(..len))
        .ok_or_else(|| anyhow!("TIFF strip at {at} runs {len} bytes past the end of the file"))
}

/// Split packed rows into one index per pixel.
///
/// At 8 bits that is the bytes as they stand. Below it the samples are packed
/// most significant bits first and each row is padded out to a whole number of
/// bytes, so the padding has to be stepped over row by row.
fn unpack_samples(data: &[u8], bits: usize, width: usize, height: usize) -> Vec<u8> {
    let row_bytes = (width * bits).div_ceil(8);
    let mut indices = Vec::with_capacity(width * height);
    for y in 0..height {
        let row = &data[y * row_bytes..(y + 1) * row_bytes];
        if bits == 8 {
            indices.extend_from_slice(row);
            continue;
        }
        let mask = (1u16 << bits) - 1;
        for x in 0..width {
            let bit = x * bits;
            let shift = 8 - bits - (bit % 8);
            indices.push(((u16::from(row[bit / 8]) >> shift) & mask) as u8);
        }
    }
    indices
}

/// Reads codes of a given width, most significant bit first, which is how TIFF
/// packs LZW (and the opposite of how GIF does).
struct Codes<'a> {
    data: &'a [u8],
    /// Position in bits from the start of `data`.
    pos: usize,
}

impl Codes<'_> {
    fn next(&mut self, width: u32) -> Option<u16> {
        let mut code = 0u32;
        for _ in 0..width {
            let byte = *self.data.get(self.pos / 8)?;
            let bit = (byte >> (7 - self.pos % 8)) & 1;
            code = code << 1 | u32::from(bit);
            self.pos += 1;
        }
        Some(code as u16)
    }
}

/// Decode a TIFF LZW strip into at most `expected` bytes.
///
/// The dictionary starts as the 256 single bytes plus the two reserved codes,
/// and grows by one string per code decoded: the previous string plus the first
/// byte of this one. Code width follows the dictionary's size, and TIFF's
/// variant grows it one code *early* — a decoder that waits for the table to be
/// genuinely full reads every later code shifted, which is the classic way to
/// get garbage out of an otherwise correct implementation.
fn lzw_decode(data: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut codes = Codes { data, pos: 0 };
    let mut table: Vec<Vec<u8>> = Vec::with_capacity(1 << LZW_MAX_WIDTH);
    let mut width = LZW_MIN_WIDTH;
    let mut previous: Option<Vec<u8>> = None;
    let mut out = Vec::with_capacity(expected);

    let reset = |table: &mut Vec<Vec<u8>>| {
        table.clear();
        table.extend((0..=u8::MAX).map(|b| vec![b]));
        // The two reserved codes hold no string; empty entries stand in for
        // them so that a string's code stays its index in the table, and the
        // first code the encoder assigns is `LZW_FIRST_CODE`.
        table.resize(LZW_FIRST_CODE, Vec::new());
    };
    reset(&mut table);

    while out.len() < expected {
        let Some(code) = codes.next(width) else { break };
        match code {
            LZW_EOI => break,
            LZW_CLEAR => {
                reset(&mut table);
                width = LZW_MIN_WIDTH;
                previous = None;
            }
            _ => {
                let string = match table.get(code as usize) {
                    Some(known) => known.clone(),
                    // The one code that may be used before it is defined: the
                    // encoder emits it for a run whose string it has just
                    // added, which is the previous string plus its own first
                    // byte.
                    None if code as usize == table.len() => {
                        let previous = previous
                            .as_ref()
                            .ok_or_else(|| anyhow!("TIFF LZW starts with an undefined code"))?;
                        let mut string = previous.clone();
                        string.push(previous[0]);
                        string
                    }
                    None => bail!("TIFF LZW code {code} is past the end of the table"),
                };
                // Past the widest code there is no room for another string and
                // the encoder owes us a clear code, so the table stops growing.
                if let Some(previous) = previous
                    && table.len() < 1 << LZW_MAX_WIDTH
                {
                    let mut entry = previous;
                    entry.push(string[0]);
                    table.push(entry);
                }
                out.extend_from_slice(&string);
                previous = Some(string);
                // One code early, as above: at 511 entries the next code the
                // encoder writes is already ten bits wide.
                width = match table.len() {
                    511 => 10,
                    1023 => 11,
                    2047 => 12,
                    _ => width,
                };
            }
        }
    }
    Ok(out)
}

/// One-line description of a palette TIFF's size and depth, for the frontend's
/// info display — e.g. `TIFF 320x200 (256 colors)`. Best effort: a file whose
/// directory says nothing useful is just called a TIFF.
pub fn describe(bytes: &[u8]) -> String {
    let name = "TIFF";
    let described = || -> Result<String> {
        let ifd = Ifd::first(bytes)?;
        let bits = ifd.value_or(TAG_BITS_PER_SAMPLE, 1)?.min(u8::BITS);
        Ok(format!(
            "{name} {}x{} ({} colors)",
            ifd.value(TAG_IMAGE_WIDTH)?,
            ifd.value(TAG_IMAGE_LENGTH)?,
            1u32 << bits
        ))
    };
    described().unwrap_or_else(|_| name.into())
}

#[cfg(test)]
#[path = "tests/tiff_pal_tests.rs"]
mod tests;
