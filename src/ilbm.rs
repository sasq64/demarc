use anyhow::{Result, anyhow, bail};
use image::RgbaImage;
use std::{
    fs::{self},
    path::Path,
};

struct Chunk<'a> {
    data: &'a [u8],
}

impl<'a> Chunk<'a> {
    pub fn id(&self) -> String {
        String::from_utf8_lossy(&self.data[0..4]).into()
    }
    pub fn size(&self) -> usize {
        let len = u32::from_be_bytes(self.data[4..8].try_into().unwrap());
        len as usize
    }
    pub fn data(&self) -> &[u8] {
        &self.data[8..8 + self.size()]
    }

    /// Iterate over the subchunks contained in this chunk.
    pub fn chunks(&self) -> Chunks<'a> {
        let id = self.id();
        let start = if id == "FORM" || id == "LIST" || id == "CAT " {
            12
        } else {
            8
        };
        Chunks {
            data: self.data,
            offset: start,
        }
    }

    /// Find the first direct subchunk with the given four-character id.
    fn find(&self, id: &str) -> Option<Chunk<'a>> {
        self.chunks().find(|c| c.id() == id)
    }
}

/// Iterator over the subchunks of a [`Chunk`], produced by [`Chunk::chunks`].
struct Chunks<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> Iterator for Chunks<'a> {
    type Item = Chunk<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 8 > self.data.len() {
            return None;
        }
        let chunk = Chunk {
            data: &self.data[self.offset..],
        };
        let size = chunk.size();
        // Data is padded to an even byte boundary; the pad byte is not
        // counted in the size field.
        let advance = 8 + size + (size & 1);
        self.offset += advance;
        Some(chunk)
    }
}

/// Parsed contents of a BMHD (bitmap header) chunk.
#[derive(Clone, Copy)]
struct BmHeader {
    width: u16,
    height: u16,
    num_planes: u8,
    masking: u8,
    compression: u8,
}

impl BmHeader {
    fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 20 {
            bail!("BMHD chunk too small");
        }
        Ok(BmHeader {
            width: u16::from_be_bytes([data[0], data[1]]),
            height: u16::from_be_bytes([data[2], data[3]]),
            num_planes: data[8],
            masking: data[9],
            compression: data[10],
        })
    }
}

// CAMG viewport-mode flags relevant to how pixels are decoded.
const CAMG_HAM: u32 = 0x0800;
const CAMG_EHB: u32 = 0x0080;
// CAMG viewport-mode flags describing the display resolution, used to correct
// the pixel aspect ratio (Amiga pixels aren't square in every mode).
const CAMG_HIRES: u32 = 0x8000;
const CAMG_SHRES: u32 = 0x0020;
const CAMG_LACE: u32 = 0x0004;

/// Integer pixel-replication factors that turn an Amiga image's non-square
/// pixels into square ones for display. On the Amiga a lores pixel is roughly
/// square, a hires pixel is half as wide (so hires images are stretched
/// vertically), and an interlaced pixel is half as tall (so lores-interlace
/// images are stretched horizontally). A hires-interlace pixel is square again.
///
/// The resolution comes from the CAMG viewport mode when present; only the
/// HIRES/SHRES/LACE bits are trusted because old writers leave garbage in the
/// other bits. When no CAMG mode is present the size is inferred from the pixel
/// dimensions (the classic 320x512 -> 640x512 and 640x256 -> 640x512 cases).
/// PBM (PC DeluxePaint) images always have square pixels and are left alone.
fn display_scale(form_type: &str, width: usize, height: usize, camg: u32) -> (usize, usize) {
    if form_type == "PBM " {
        return (1, 1);
    }
    let mode = camg & (CAMG_HIRES | CAMG_SHRES | CAMG_LACE);
    let (hires, lace) = if mode != 0 {
        (camg & (CAMG_HIRES | CAMG_SHRES) != 0, camg & CAMG_LACE != 0)
    } else {
        // No usable viewport mode: fall back to the dimensions. Hires screens
        // are ~640+ wide; interlaced screens are ~400+ lines tall.
        (width >= 512, height >= 400)
    };
    match (hires, lace) {
        (true, false) => (1, 2), // hires: thin pixels, double the height
        (false, true) => (2, 1), // lores interlace: wide pixels, double the width
        _ => (1, 1),             // lores or hires-interlace: already square
    }
}

/// Replicate each element of a `width` x `height` grid `sx` times horizontally
/// and `sy` times vertically (nearest-neighbour upscale). Used to apply the
/// aspect-ratio correction from [`display_scale`] to pixels or palette indices.
fn scale_grid<T: Copy>(src: &[T], width: usize, height: usize, sx: usize, sy: usize) -> Vec<T> {
    if sx == 1 && sy == 1 {
        return src.to_vec();
    }
    let nw = width * sx;
    let mut out = Vec::with_capacity(nw * height * sy);
    for y in 0..height * sy {
        let row = (y / sy) * width;
        for x in 0..nw {
            out.push(src[row + x / sx]);
        }
    }
    out
}

/// Decompress a ByteRun1 (PackBits) encoded body into `expected` bytes.
fn unpack_byterun1(src: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected);
    let mut i = 0;
    while i < src.len() && out.len() < expected {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            // Copy the next n + 1 bytes literally.
            let count = n as usize + 1;
            let end = i + count;
            if end > src.len() {
                bail!("ByteRun1 literal run runs past end of data");
            }
            out.extend_from_slice(&src[i..end]);
            i = end;
        } else if n != -128 {
            // Repeat the next byte -n + 1 times. (-128 is a no-op.)
            let count = (-(n as i32)) as usize + 1;
            if i >= src.len() {
                bail!("ByteRun1 repeat run runs past end of data");
            }
            let byte = src[i];
            i += 1;
            out.resize(out.len() + count, byte);
        }
    }
    Ok(out)
}

/// A colour-cycling range (CRNG chunk), as used by DeluxePaint. The colours in
/// palette registers `low..=high` are rotated over time to animate the image.
#[derive(Debug, Clone, Copy)]
pub struct CycleRange {
    /// Lowest palette register in the range.
    pub low: u8,
    /// Highest palette register in the range.
    pub high: u8,
    /// Cycling speed. In CRNG units, `16384` == 60 steps per second, so the
    /// step rate in Hz is `rate * 60 / 16384`.
    pub rate: u16,
    /// Whether cycling is enabled for this range (CRNG flag bit 0).
    pub active: bool,
    /// Whether the cycle direction is reversed (CRNG flag bit 1).
    pub reverse: bool,
}

/// A paletted ILBM image plus any colour-cycling ranges. Pixels are stored as
/// palette indices (row-major, one byte each) so the palette can be rotated at
/// display time. Only produced for plain-palette images (not HAM).
pub struct IndexedImage {
    pub width: u32,
    pub height: u32,
    /// RGB triplets, one per palette register.
    pub palette: Vec<[u8; 3]>,
    /// One palette index per pixel, `width * height` bytes, row-major.
    pub indices: Vec<u8>,
    /// Colour-cycling ranges declared by CRNG chunks.
    pub ranges: Vec<CycleRange>,
}

/// Pixel data decoded from a bitmap, before palette resolution.
enum Content {
    /// Paletted pixels: one register index per pixel, plus the palette (already
    /// expanded to 64 entries for Extra-HalfBrite). `is_ham` marks Hold-And-
    /// Modify images, whose indices aren't plain palette lookups.
    Indexed {
        num_planes: usize,
        is_ham: bool,
        palette: Vec<[u8; 3]>,
        indices: Vec<u8>,
        /// Per-scanline palettes (one entry per row) for images whose colours
        /// change down the screen — CTBL/BEAM dynamic tables, sliced-HAM
        /// (SHAM), or palette-change (PCHG) chunks. When present, row `y` uses
        /// `line_palettes[y]` in place of `palette`. Such images can't be shown
        /// as a single indexed frame, so they take the fixed-RGBA path.
        line_palettes: Option<Vec<Vec<[u8; 3]>>>,
    },
    /// Direct truecolour pixels, `width * height` RGB triplets, row-major.
    Rgb(Vec<[u8; 3]>),
}

/// Parsed bitmap ready to be turned into RGBA or an [`IndexedImage`]. The
/// dimensions and pixel content are the image's native (as-stored) size; the
/// scale factors correct the pixel aspect ratio and are applied when the image
/// is realised (see [`display_scale`]).
struct Parsed {
    width: usize,
    height: usize,
    /// Horizontal / vertical pixel-replication factors for aspect correction.
    xscale: usize,
    yscale: usize,
    ranges: Vec<CycleRange>,
    content: Content,
}

/// Decode an interleaved planar body (ILBM `BODY`) into one palette index per
/// pixel (row-major). Only valid for up to 8 planes, so indices fit in a byte.
fn decode_indices(
    width: usize,
    height: usize,
    num_planes: usize,
    row_bytes: usize,
    planes_per_row: usize,
    planar: &[u8],
) -> Vec<u8> {
    let mut indices = vec![0u8; width * height];
    for y in 0..height {
        let row_base = y * planes_per_row * row_bytes;
        for x in 0..width {
            let byte_idx = x / 8;
            let bit = 7 - (x % 8);
            let mut index = 0u32;
            for p in 0..num_planes {
                let b = planar[row_base + p * row_bytes + byte_idx];
                index |= (((b >> bit) & 1) as u32) << p;
            }
            // At most 8 planes (checked by the caller), so it fits in a byte.
            indices[y * width + x] = index as u8;
        }
    }
    indices
}

/// Decode a deep (24- or 32-plane) interleaved planar body directly to RGB.
/// Planes 0..8 form the red byte, 8..16 green, 16..24 blue; any higher planes
/// (an alpha byte for 32-plane images) are ignored.
fn decode_deep(
    width: usize,
    height: usize,
    num_planes: usize,
    row_bytes: usize,
    planes_per_row: usize,
    planar: &[u8],
) -> Vec<[u8; 3]> {
    let mut pixels = vec![[0u8; 3]; width * height];
    for y in 0..height {
        let row_base = y * planes_per_row * row_bytes;
        for x in 0..width {
            let byte_idx = x / 8;
            let bit = 7 - (x % 8);
            let mut value = 0u32;
            for p in 0..num_planes {
                let b = planar[row_base + p * row_bytes + byte_idx];
                value |= (((b >> bit) & 1) as u32) << p;
            }
            pixels[y * width + x] = [value as u8, (value >> 8) as u8, (value >> 16) as u8];
        }
    }
    pixels
}

/// Decode an ACBM `ABIT` body into one palette index per pixel. Unlike ILBM,
/// the bitplanes are stored contiguously (all of plane 0, then all of plane 1,
/// …) and are never compressed.
fn decode_acbm(
    width: usize,
    height: usize,
    num_planes: usize,
    row_bytes: usize,
    abit: &[u8],
) -> Result<Vec<u8>> {
    let plane_size = row_bytes * height;
    if abit.len() < plane_size * num_planes {
        bail!(
            "ACBM body too small: got {}, expected {}",
            abit.len(),
            plane_size * num_planes
        );
    }
    let mut indices = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let byte_idx = x / 8;
            let bit = 7 - (x % 8);
            let mut index = 0u32;
            for p in 0..num_planes {
                let b = abit[p * plane_size + y * row_bytes + byte_idx];
                index |= (((b >> bit) & 1) as u32) << p;
            }
            indices[y * width + x] = index as u8;
        }
    }
    Ok(indices)
}

/// Decode a PBM (DeluxePaint PC) chunky body into one palette index per pixel.
/// Each pixel is already a byte; rows are padded to an even byte width.
fn decode_pbm(width: usize, height: usize, compression: u8, body: &[u8]) -> Result<Vec<u8>> {
    let row_bytes = width + (width & 1);
    let expected = row_bytes * height;
    let raw = match compression {
        0 => body.to_vec(),
        1 => unpack_byterun1(body, expected)?,
        c => bail!("unsupported PBM compression: {c}"),
    };
    if raw.len() < expected {
        bail!(
            "decoded PBM body too small: got {}, expected {expected}",
            raw.len()
        );
    }
    let mut indices = vec![0u8; width * height];
    for y in 0..height {
        let src = &raw[y * row_bytes..y * row_bytes + width];
        indices[y * width..(y + 1) * width].copy_from_slice(src);
    }
    Ok(indices)
}

/// Decode an Impulse RGBN (12-bit) body into RGB pixels. Pixels are stored as
/// RLE runs of a 16-bit word: `rrrr gggg bbbb g c c c` where the top 12 bits
/// are the 4-bit R/G/B components, and the low 3 bits are a run length (bit 3
/// is genlock, ignored). A zero run length escapes to a byte, then a word.
fn decode_rgbn(width: usize, height: usize, body: &[u8]) -> Vec<[u8; 3]> {
    let total = width * height;
    let mut out = Vec::with_capacity(total);
    let mut i = 0;
    while out.len() < total && i + 2 <= body.len() {
        let w = u16::from_be_bytes([body[i], body[i + 1]]);
        i += 2;
        let r = ((w >> 12) & 0xf) as u8;
        let g = ((w >> 8) & 0xf) as u8;
        let b = ((w >> 4) & 0xf) as u8;
        let count = read_run_len((w & 0x7) as usize, body, &mut i);
        // Replicate the 4-bit component into both nibbles for a full 0..255 range.
        let rgb = [r << 4 | r, g << 4 | g, b << 4 | b];
        for _ in 0..count {
            if out.len() >= total {
                break;
            }
            out.push(rgb);
        }
    }
    out.resize(total, [0, 0, 0]);
    out
}

/// Decode an Impulse RGB8 (24-bit) body into RGB pixels. Pixels are stored as
/// RLE runs of four bytes: `r g b c`, where `c`'s low 7 bits are the run length
/// (bit 7 is genlock, ignored). A zero run length escapes to a byte, then word.
fn decode_rgb8(width: usize, height: usize, body: &[u8]) -> Vec<[u8; 3]> {
    let total = width * height;
    let mut out = Vec::with_capacity(total);
    let mut i = 0;
    while out.len() < total && i + 4 <= body.len() {
        let rgb = [body[i], body[i + 1], body[i + 2]];
        let inline = (body[i + 3] & 0x7f) as usize;
        i += 4;
        let count = read_run_len(inline, body, &mut i);
        for _ in 0..count {
            if out.len() >= total {
                break;
            }
            out.push(rgb);
        }
    }
    out.resize(total, [0, 0, 0]);
    out
}

/// Resolve an RGBN/RGB8 run length: if the in-header length is zero, read a
/// byte; if that is also zero, read a big-endian word. `i` points just past the
/// run header and is advanced over any escape bytes consumed.
fn read_run_len(inline: usize, body: &[u8], i: &mut usize) -> usize {
    if inline != 0 {
        return inline;
    }
    let byte = body.get(*i).copied().unwrap_or(0) as usize;
    *i += 1;
    if byte != 0 {
        return byte;
    }
    let word = body
        .get(*i..*i + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
        .unwrap_or(0);
    *i += 2;
    word
}

/// Collect the colour-cycling ranges from every CRNG chunk in the FORM.
fn parse_ranges(form: &Chunk) -> Vec<CycleRange> {
    form.chunks()
        .filter(|c| c.id() == "CRNG")
        .filter_map(|c| {
            let d = c.data();
            // CRNG: pad(2) rate(2) flags(2) low(1) high(1).
            if d.len() < 8 {
                return None;
            }
            let rate = u16::from_be_bytes([d[2], d[3]]);
            let flags = u16::from_be_bytes([d[4], d[5]]);
            Some(CycleRange {
                low: d[6],
                high: d[7],
                rate,
                active: flags & 1 != 0,
                reverse: flags & 2 != 0,
            })
        })
        .collect()
}

/// Expand a 12-bit `$0RGB` colour word (4 bits per component) to 8-bit RGB by
/// replicating each nibble into both halves of its byte.
fn rgb12(w: u16) -> [u8; 3] {
    let r = ((w >> 8) & 0xf) as u8;
    let g = ((w >> 4) & 0xf) as u8;
    let b = (w & 0xf) as u8;
    [r << 4 | r, g << 4 | g, b << 4 | b]
}

/// Build per-scanline palettes from a flat table of 12-bit colour words, one
/// group of colours per row (CTBL "dynamic colour table" and the equivalent
/// BEAM chunk). The registers-per-row count is taken from the chunk size, so
/// 16-register images (including HAM6, which has 16 base registers) and wider
/// tables both work. Trailing rows past the table reuse the last row's colours.
fn parse_color_table(data: &[u8], height: usize) -> Vec<Vec<[u8; 3]>> {
    let regs = (data.len() / 2) / height.max(1);
    if regs == 0 {
        return Vec::new();
    }
    (0..height)
        .map(|y| {
            (0..regs)
                .map(|i| {
                    let off = (y * regs + i) * 2;
                    data.get(off..off + 2)
                        .map(|b| rgb12(u16::from_be_bytes([b[0], b[1]])))
                        .unwrap_or([0, 0, 0])
                })
                .collect()
        })
        .collect()
}

/// Build per-scanline palettes from a SHAM (sliced-HAM) chunk: a version word
/// followed by a run of 16-colour, 12-bit palettes. There is one palette per
/// (possibly doubled) scanline, so map each output row onto its palette by
/// scaling — a 256-palette SHAM over a 512-line image gives one palette per two
/// rows, matching how the hardware reloads colours every other line.
fn parse_sham(data: &[u8], height: usize) -> Vec<Vec<[u8; 3]>> {
    // Skip the 2-byte version field; the rest is 16 words per palette.
    let body = data.get(2..).unwrap_or(&[]);
    let num = (body.len() / 2) / 16;
    if num == 0 {
        return Vec::new();
    }
    (0..height)
        .map(|y| {
            let p = (y * num / height.max(1)).min(num - 1);
            (0..16)
                .map(|i| {
                    let off = (p * 16 + i) * 2;
                    body.get(off..off + 2)
                        .map(|b| rgb12(u16::from_be_bytes([b[0], b[1]])))
                        .unwrap_or([0, 0, 0])
                })
                .collect()
        })
        .collect()
}

/// Build per-scanline palettes from a PCHG (palette-change) chunk, starting
/// from `base` and applying the per-line register changes cumulatively. Only
/// the uncompressed, small (12-bit) change format is handled; anything else
/// (Huffman-compressed or 24-bit "big" changes) returns `None` so the caller
/// falls back to the static palette.
fn parse_pchg(data: &[u8], base: &[[u8; 3]], height: usize) -> Option<Vec<Vec<[u8; 3]>>> {
    if data.len() < 20 {
        return None;
    }
    let compression = u16::from_be_bytes([data[0], data[1]]);
    let flags = u16::from_be_bytes([data[2], data[3]]);
    let start_line = i16::from_be_bytes([data[4], data[5]]) as i32;
    let line_count = u16::from_be_bytes([data[6], data[7]]) as usize;
    const PCHGF_SMALL: u16 = 1;
    if compression != 0 || flags & PCHGF_SMALL == 0 {
        return None;
    }

    // A line-change bitmap follows the 20-byte header: `line_count` bits packed
    // into big-endian 32-bit longwords, bit (31 - i%32) of longword i/32 marking
    // that scanline `start_line + i` carries a SmallLineChanges record.
    let mut off = 20;
    let mask_bytes = line_count.div_ceil(32) * 4;
    let mask = data.get(off..off + mask_bytes)?;
    off += mask_bytes;

    // Running palette, padded to 32 registers so 16..31 changes have somewhere
    // to land even for shallow images.
    let mut pal = base.to_vec();
    pal.resize(32, [0, 0, 0]);

    let mut lines = Vec::with_capacity(height);
    for y in 0..height {
        let i = y as i32 - start_line;
        if i >= 0 && (i as usize) < line_count {
            let idx = i as usize;
            let long = u32::from_be_bytes(mask[idx / 32 * 4..idx / 32 * 4 + 4].try_into().unwrap());
            if (long >> (31 - idx % 32)) & 1 != 0 {
                // SmallLineChanges: count for registers 0-15, count for 16-31,
                // then that many change words (register in the top nibble, a
                // 4:4:4 RGB value in the low 12 bits).
                let cc16 = *data.get(off)? as usize;
                let cc32 = *data.get(off + 1)? as usize;
                off += 2;
                for j in 0..cc16 + cc32 {
                    let w = u16::from_be_bytes(data.get(off..off + 2)?.try_into().unwrap());
                    off += 2;
                    let reg = ((w >> 12) & 0xf) as usize + if j < cc16 { 0 } else { 16 };
                    if reg < pal.len() {
                        pal[reg] = rgb12(w & 0x0fff);
                    }
                }
            }
        }
        lines.push(pal.clone());
    }
    Some(lines)
}

/// Attach per-scanline palettes to an [`Content::Indexed`] image from whichever
/// dynamic-palette chunk (SHAM, CTBL, BEAM, or PCHG) the FORM carries.
fn attach_line_palettes(form: &Chunk, height: usize, content: Content) -> Content {
    let Content::Indexed {
        num_planes,
        is_ham,
        palette,
        indices,
        line_palettes: _,
    } = content
    else {
        return content;
    };
    let line_palettes = if let Some(c) = form.find("SHAM") {
        Some(parse_sham(c.data(), height))
    } else if let Some(c) = form.find("CTBL").or_else(|| form.find("BEAM")) {
        Some(parse_color_table(c.data(), height))
    } else if let Some(c) = form.find("PCHG") {
        parse_pchg(c.data(), &palette, height)
    } else {
        None
    };
    // Drop an empty result so a malformed chunk falls back to the static palette.
    let line_palettes = line_palettes.filter(|lp| !lp.is_empty());
    Content::Indexed {
        num_planes,
        is_ham,
        palette,
        indices,
        line_palettes,
    }
}

/// Decode the pixel content of an interleaved-planar ILBM `BODY`, dispatching
/// on the plane count: up to 8 planes yields palette indices, 24/32 planes a
/// deep truecolour image.
fn decode_ilbm(form: &Chunk, header: &BmHeader, width: usize, height: usize) -> Result<Content> {
    let body = form
        .find("BODY")
        .ok_or_else(|| anyhow!("missing BODY chunk"))?;
    let num_planes = header.num_planes as usize;

    // Bytes per bitplane per scanline: rows are padded to a 16-bit boundary.
    let row_bytes = width.div_ceil(16) * 2;
    // A mask plane (masking == 1) is stored as an extra plane per row.
    let planes_per_row = num_planes + if header.masking == 1 { 1 } else { 0 };
    let expected = row_bytes * planes_per_row * height;

    let planar = match header.compression {
        0 => body.data().to_vec(),
        1 => unpack_byterun1(body.data(), expected)?,
        c => bail!("unsupported compression: {c}"),
    };
    if planar.len() < expected {
        bail!(
            "decoded body too small: got {}, expected {expected}",
            planar.len()
        );
    }

    match num_planes {
        1..=8 => Ok(Content::Indexed {
            num_planes,
            is_ham: false, // filled in by the caller from CAMG
            palette: Vec::new(),
            indices: decode_indices(
                width,
                height,
                num_planes,
                row_bytes,
                planes_per_row,
                &planar,
            ),
            line_palettes: None,
        }),
        24 | 32 => Ok(Content::Rgb(decode_deep(
            width,
            height,
            num_planes,
            row_bytes,
            planes_per_row,
            &planar,
        ))),
        n => bail!("unsupported plane count: {n}"),
    }
}

/// Parse an in-memory IFF image (ILBM, PBM, ACBM, or Impulse RGB8/RGBN) into
/// its pixel content and any colour-cycling ranges.
fn parse(bytes: &[u8]) -> Result<Parsed> {
    // A valid FORM needs at least the 8-byte chunk header plus a 4-byte type.
    if bytes.len() < 12 {
        bail!("not an IFF FORM: too short ({} bytes)", bytes.len());
    }
    let form = Chunk { data: bytes };
    if form.id() != "FORM" {
        bail!("not an IFF FORM");
    }
    let form_type = form
        .data()
        .get(0..4)
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();

    let bmhd = form
        .find("BMHD")
        .ok_or_else(|| anyhow!("missing BMHD chunk"))?;
    let header = BmHeader::parse(bmhd.data())?;
    let width = header.width as usize;
    let height = header.height as usize;
    let num_planes = header.num_planes as usize;
    if width == 0 || height == 0 {
        bail!("zero-sized image ({width}x{height})");
    }

    let camg = form
        .find("CAMG")
        .and_then(|c| {
            c.data()
                .get(0..4)
                .map(|b| u32::from_be_bytes(b.try_into().unwrap()))
        })
        .unwrap_or(0);
    let is_ham = camg & CAMG_HAM != 0;
    let is_ehb = camg & CAMG_EHB != 0;

    // Colour map: RGB triplets, one per palette entry. Only used by the paletted
    // formats; RGB8/RGBN carry colour per pixel.
    let mut palette: Vec<[u8; 3]> = form
        .find("CMAP")
        .map(|c| {
            c.data()
                .chunks_exact(3)
                .map(|c| [c[0], c[1], c[2]])
                .collect()
        })
        .unwrap_or_default();

    // Row width in bytes for the planar (ILBM/ACBM) formats.
    let row_bytes = width.div_ceil(16) * 2;

    let content = match form_type.as_str() {
        "ILBM" => decode_ilbm(&form, &header, width, height)?,
        "PBM " => {
            let body = form
                .find("BODY")
                .ok_or_else(|| anyhow!("missing BODY chunk"))?;
            Content::Indexed {
                num_planes,
                is_ham: false,
                palette: Vec::new(),
                indices: decode_pbm(width, height, header.compression, body.data())?,
                line_palettes: None,
            }
        }
        "ACBM" => {
            if num_planes == 0 || num_planes > 8 {
                bail!("unsupported ACBM plane count: {num_planes}");
            }
            let abit = form
                .find("ABIT")
                .ok_or_else(|| anyhow!("missing ABIT chunk"))?;
            Content::Indexed {
                num_planes,
                is_ham: false,
                palette: Vec::new(),
                indices: decode_acbm(width, height, num_planes, row_bytes, abit.data())?,
                line_palettes: None,
            }
        }
        "RGBN" => {
            let body = form
                .find("BODY")
                .ok_or_else(|| anyhow!("missing BODY chunk"))?;
            Content::Rgb(decode_rgbn(width, height, body.data()))
        }
        "RGB8" => {
            let body = form
                .find("BODY")
                .ok_or_else(|| anyhow!("missing BODY chunk"))?;
            Content::Rgb(decode_rgb8(width, height, body.data()))
        }
        other => bail!("unsupported IFF form type: {other:?}"),
    };

    // Finish the paletted content: attach the colour map (expanding it for
    // Extra-HalfBrite) and the HAM flag, both drawn from the FORM's shared
    // header chunks rather than the per-format decoders.
    let content = match content {
        Content::Rgb(px) => Content::Rgb(px),
        Content::Indexed {
            num_planes,
            indices,
            ..
        } => {
            // Extra-HalfBrite: expand the palette to 64 registers so a plain
            // index lookup yields the upper half-bright colours, letting EHB
            // use the same indexed path as any other palette image.
            if is_ehb {
                palette.resize(32, [0, 0, 0]);
                let base: Vec<[u8; 3]> = palette[..32].to_vec();
                palette.extend(base.iter().map(|c| [c[0] >> 1, c[1] >> 1, c[2] >> 1]));
            }
            let content = Content::Indexed {
                num_planes,
                is_ham,
                palette,
                indices,
                line_palettes: None,
            };
            // Pick up any dynamic (per-scanline) palette from a SHAM/CTBL/BEAM/
            // PCHG chunk now that the base palette is finalised.
            attach_line_palettes(&form, height, content)
        }
    };

    let (xscale, yscale) = display_scale(&form_type, width, height, camg);

    Ok(Parsed {
        width,
        height,
        xscale,
        yscale,
        ranges: parse_ranges(&form),
        content,
    })
}

/// Decode an in-memory IFF image into an RGBA image (colours resolved, cycling
/// not applied).
pub fn load_from_memory(bytes: &[u8]) -> Result<RgbaImage> {
    let p = parse(bytes)?;
    let mut img = RgbaImage::new(p.width as u32, p.height as u32);
    match &p.content {
        Content::Rgb(pixels) => {
            for (i, rgb) in pixels.iter().enumerate() {
                let (x, y) = ((i % p.width) as u32, (i / p.width) as u32);
                img.put_pixel(x, y, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
            }
        }
        Content::Indexed {
            num_planes,
            is_ham,
            palette,
            indices,
            line_palettes,
        } => {
            for y in 0..p.height {
                // For dynamic-palette images each row has its own palette;
                // otherwise every row shares the single global one.
                let pal = line_palettes
                    .as_ref()
                    .and_then(|lp| lp.get(y))
                    .map(|v| v.as_slice())
                    .unwrap_or(palette.as_slice());
                // HAM carries colour forward across a scanline, seeded from black.
                let mut prev = [0u8, 0, 0];
                for x in 0..p.width {
                    let index = indices[y * p.width + x] as usize;
                    let rgb = if *is_ham {
                        ham_pixel(index, *num_planes, pal, &mut prev)
                    } else {
                        pal.get(index).copied().unwrap_or([0, 0, 0])
                    };
                    img.put_pixel(
                        x as u32,
                        y as u32,
                        image::Rgba([rgb[0], rgb[1], rgb[2], 255]),
                    );
                }
            }
        }
    }
    // Correct the pixel aspect ratio by replicating whole pixels; line palettes
    // and HAM were already resolved above, so this is a plain nearest upscale.
    if p.xscale == 1 && p.yscale == 1 {
        return Ok(img);
    }
    let pixels: Vec<[u8; 4]> = img.pixels().map(|p| p.0).collect();
    let scaled = scale_grid(&pixels, p.width, p.height, p.xscale, p.yscale);
    let raw: Vec<u8> = scaled.into_iter().flatten().collect();
    Ok(RgbaImage::from_raw(
        (p.width * p.xscale) as u32,
        (p.height * p.yscale) as u32,
        raw,
    )
    .expect("scaled buffer matches dimensions"))
}

/// Decode an in-memory IFF image into an [`IndexedImage`], preserving the
/// palette and per-pixel indices so colour cycling can be applied at display
/// time. Fails for HAM and truecolour images, whose pixels aren't plain palette
/// lookups.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    let p = parse(bytes)?;
    match p.content {
        Content::Indexed { is_ham: true, .. } => {
            bail!("HAM images can't be displayed as an indexed (colour-cycled) image")
        }
        Content::Indexed {
            line_palettes: Some(_),
            ..
        } => {
            bail!("dynamic-palette images can't be displayed as an indexed (colour-cycled) image")
        }
        Content::Rgb(_) => {
            bail!("truecolour images can't be displayed as an indexed (colour-cycled) image")
        }
        Content::Indexed {
            palette, indices, ..
        } => Ok(IndexedImage {
            width: (p.width * p.xscale) as u32,
            height: (p.height * p.yscale) as u32,
            // Aspect correction replicates indices, not colours, so the palette
            // (and any colour cycling applied to it) is untouched.
            indices: scale_grid(&indices, p.width, p.height, p.xscale, p.yscale),
            palette,
            ranges: p.ranges,
        }),
    }
}

/// Resolve one Hold-And-Modify pixel, updating the running colour.
fn ham_pixel(index: usize, num_planes: usize, palette: &[[u8; 3]], prev: &mut [u8; 3]) -> [u8; 3] {
    // The two high planes are control bits; the rest select/modify a component.
    let data_bits = num_planes - 2;
    let control = index >> data_bits;
    let value = index & ((1 << data_bits) - 1);
    // Expand the data value (4 bits for HAM6, 6 for HAM8) to a full 8-bit
    // component, replicating the high bits into the low ones.
    let comp = ((value << (8 - data_bits)) | (value >> (2 * data_bits - 8))) as u8;
    match control {
        0 => *prev = palette.get(value).copied().unwrap_or([0, 0, 0]),
        1 => prev[2] = comp, // modify blue
        2 => prev[0] = comp, // modify red
        _ => prev[1] = comp, // modify green
    }
    *prev
}

/// Load an ILBM/IFF image from a file into an RGBA image.
pub fn load(path: impl AsRef<Path>) -> Result<RgbaImage> {
    load_from_memory(&fs::read(path.as_ref())?)
}

/// Load an ILBM/IFF image from a file into an [`IndexedImage`] (see
/// [`load_indexed_from_memory`]).
pub fn load_indexed(path: impl AsRef<Path>) -> Result<IndexedImage> {
    load_indexed_from_memory(&fs::read(path.as_ref())?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn get_path(name: &str) -> PathBuf {
        Path::new("testdata/iffILBM").join(name)
    }

    /// Load a test image from the `iffILBM/` corpus and assert it decodes to the
    /// expected size and isn't a single flat colour (which would signal a
    /// mis-decode). Returns the decoded image for further checks.
    fn check(file: &str, w: u32, h: u32) -> RgbaImage {
        let img = load(get_path(file)).unwrap_or_else(|e| panic!("failed to load {file}: {e}"));
        assert_eq!(img.dimensions(), (w, h), "wrong dimensions for {file}");
        let first = img.get_pixel(0, 0);
        assert!(
            img.pixels().any(|p| p != first),
            "decoded {file} is a single flat colour"
        );
        img
    }

    #[test]
    fn test_ilbm() {
        let img = load("testdata/test.iff").unwrap();
        assert_eq!(img.dimensions(), (640, 512));
        // The image must not be entirely one colour.
        let first = img.get_pixel(0, 0);
        assert!(img.pixels().any(|p| p != first), "decoded image is blank");
    }

    /// Plain paletted ILBM (interleaved planar, ByteRun1 compressed).
    #[test]
    fn test_ilbm_paletted() {
        check("abydos.ilbm", 320, 240);
        check("Devilstar_Indyface.iff", 320, 256);
    }

    /// Uncompressed ILBM body (compression == 0).
    #[test]
    fn test_ilbm_uncompressed() {
        check("dalton-big-wheel-640x512.ilbm", 640, 512);
    }

    /// HAM6 and HAM8 hold-and-modify images.
    #[test]
    fn test_ham() {
        check("GINA", 320, 200); // HAM6
        check("FearFace.HAM8", 640, 512); // HAM8
    }

    /// Extra-HalfBrite (64 registers, upper 32 are half-bright copies). Stored
    /// 320x512 (lores interlace), stretched to 640x512 for square pixels.
    #[test]
    fn test_ehb() {
        check("DECKER-BattleMech.lbm", 640, 512);
    }

    /// masking == 2 (transparent colour, no extra mask plane) must not offset
    /// the plane layout.
    #[test]
    fn test_masked_transparent_colour() {
        check("ghost", 320, 256);
        check("agony.iff", 288, 192);
    }

    /// PBM (DeluxePaint PC chunky), both uncompressed and ByteRun1.
    #[test]
    fn test_pbm() {
        check("4xd_oz_0.ilbm", 800, 600); // compression 0
        check("water.lbm", 640, 480); // compression 1
    }

    /// ACBM (contiguous bitplanes in an ABIT chunk).
    #[test]
    fn test_acbm() {
        check("TEST.ACBM", 320, 256); // compression flag 0
        check("cover", 320, 256); // no CAMG
        check("0900.acbm", 248, 511); // HAM ACBM, lores interlace -> double width
    }

    /// Deep 24-bit ILBM (direct RGB, no palette).
    #[test]
    fn test_deep_24bit() {
        check("24.iff", 455, 341);
    }

    /// Impulse RGB8 (24-bit truecolour, RLE-compressed).
    #[test]
    fn test_rgb8() {
        check("WorldMap2.24", 224, 118);
    }

    /// Impulse RGBN (12-bit truecolour, RLE-compressed). Stored 320x400 (lores
    /// interlace), stretched to 640x400 for square pixels.
    #[test]
    fn test_rgbn() {
        check("spock.rgbn", 640, 400);
    }

    /// The indexed path should preserve one index per pixel and a usable palette
    /// for a plain paletted image.
    #[test]
    fn test_load_indexed() {
        let img = load_indexed(get_path("abydos.ilbm")).unwrap();
        assert_eq!((img.width, img.height), (320, 240));
        assert_eq!(img.indices.len(), (320 * 240) as usize);
        assert!(!img.palette.is_empty());
        // Every index must be resolvable in the palette.
        let max = *img.indices.iter().max().unwrap() as usize;
        assert!(max < img.palette.len(), "index {max} outside palette");
    }

    /// The indexed path must refuse HAM and truecolour images (their pixels are
    /// not plain palette lookups) so callers fall back to the RGBA path.
    #[test]
    fn test_indexed_rejects_non_palette() {
        assert!(load_indexed(get_path("GINA")).is_err(), "HAM");
        assert!(load_indexed(get_path("24.iff")).is_err(), "deep");
        assert!(load_indexed(get_path("WorldMap2.24")).is_err(), "RGB8");
    }

    /// Colour-cycling ranges are collected from CRNG chunks.
    #[test]
    fn test_cycle_ranges() {
        let img = load_indexed(get_path("water.lbm")).unwrap();
        assert!(!img.ranges.is_empty(), "expected CRNG ranges");
    }

    /// CTBL "dynamic colour table": a full 16-colour palette per scanline.
    #[test]
    fn test_ctbl() {
        check("amiga-ferrari.dhr", 704, 512);
        check("Seascape.dr", 704, 480);
    }

    /// BEAM: like CTBL but combined with HAM (16 base registers per line).
    #[test]
    fn test_beam_ham() {
        check("Eagle.beam", 320, 200);
    }

    /// SHAM (sliced HAM): one 16-colour palette per (doubled) scanline.
    #[test]
    fn test_sham() {
        check("mansion.sham", 320, 200); // one palette per line (lores)
        check("sham.iff", 640, 512); // per two lines; lores interlace -> double width
    }

    /// PCHG (palette change): per-line register deltas from a base palette.
    #[test]
    fn test_pchg() {
        check("Lake.mp", 630, 422);
    }

    /// Dynamic-palette images can't be a single indexed frame, so the indexed
    /// path must reject them (callers then use the fixed-RGBA path).
    #[test]
    fn test_indexed_rejects_dynamic_palette() {
        assert!(load_indexed(Path::new("testdata/iffILBM/amiga-ferrari.dhr")).is_err());
        assert!(load_indexed(Path::new("testdata/iffILBM/Lake.mp")).is_err());
    }

    /// A dynamic-palette image should genuinely use more than its base 16
    /// colours: colours must vary between the top and bottom of the frame.
    #[test]
    fn test_ctbl_varies_down_screen() {
        let img = load(Path::new("testdata/iffILBM/amiga-ferrari.dhr")).unwrap();
        let top: std::collections::HashSet<_> =
            (0..img.width()).map(|x| img.get_pixel(x, 0).0).collect();
        let bottom: std::collections::HashSet<_> = (0..img.width())
            .map(|x| img.get_pixel(x, img.height() - 1).0)
            .collect();
        assert_ne!(top, bottom, "per-scanline palette had no visible effect");
    }

    /// Aspect-ratio correction: non-square Amiga pixels are stretched to square.
    #[test]
    fn test_aspect_correction() {
        // Hires (thin pixels) is stretched vertically: 640x256 -> 640x512.
        check("aplacet.lbm", 640, 512);
        check("blackrbe.lbm", 640, 512);
        // Lores interlace (wide pixels) is stretched horizontally, covered by
        // test_ehb / test_sham (320x512 -> 640x512).
        // Already-square modes are left at their native size:
        check("Devilstar_Indyface.iff", 320, 256); // lores
        check("dalton-big-wheel-640x512.ilbm", 640, 512); // hires interlace
        check("Seascape.dr", 704, 480); // hires interlace
        // PBM (PC DeluxePaint) always has square pixels, even at hires-ish sizes.
        check("water.lbm", 640, 480);
    }

    /// `scale_grid` replicates each cell into an `sx` x `sy` block.
    #[test]
    fn test_scale_grid() {
        // 2x1 grid [1, 2] doubled horizontally -> [1, 1, 2, 2].
        assert_eq!(scale_grid(&[1u8, 2], 2, 1, 2, 1), vec![1, 1, 2, 2]);
        // 1x2 grid [1; 2] doubled vertically -> [1, 1, 2, 2].
        assert_eq!(scale_grid(&[1u8, 2], 1, 2, 1, 2), vec![1, 1, 2, 2]);
        // A no-op scale returns the input unchanged.
        assert_eq!(scale_grid(&[1u8, 2, 3, 4], 2, 2, 1, 1), vec![1, 2, 3, 4]);
    }

    /// The `display_scale` mode logic, exercised directly.
    #[test]
    fn test_display_scale() {
        assert_eq!(display_scale("ILBM", 640, 256, CAMG_HIRES), (1, 2)); // hires
        assert_eq!(display_scale("ILBM", 320, 512, CAMG_LACE), (2, 1)); // lores lace
        assert_eq!(
            display_scale("ILBM", 640, 512, CAMG_HIRES | CAMG_LACE),
            (1, 1)
        );
        assert_eq!(display_scale("ILBM", 320, 200, 0), (1, 1)); // lores
        // Garbage in the high bits must not read as a resolution.
        assert_eq!(display_scale("ILBM", 320, 200, 0x4800), (1, 1));
        // No CAMG mode: infer from dimensions (the user's fallback rule).
        assert_eq!(display_scale("ILBM", 320, 512, 0), (2, 1));
        assert_eq!(display_scale("ILBM", 640, 256, 0), (1, 2));
        // PBM is always square, even at hires dimensions.
        assert_eq!(display_scale("PBM ", 640, 480, 0), (1, 1));
    }

    /// Non-IFF and unsupported forms should error rather than panic.
    #[test]
    fn test_rejects_garbage() {
        assert!(load_from_memory(b"not an iff file at all").is_err());
        assert!(load_from_memory(&[]).is_err());
    }
}
