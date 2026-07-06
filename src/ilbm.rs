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

/// Parsed bitmap ready to be turned into RGBA or an [`IndexedImage`].
struct Parsed {
    width: usize,
    height: usize,
    num_planes: usize,
    is_ham: bool,
    /// Palette, already expanded to 64 entries for Extra-HalfBrite images so a
    /// plain index lookup covers the upper half-bright registers.
    palette: Vec<[u8; 3]>,
    /// One palette index per pixel, `width * height` bytes.
    indices: Vec<u8>,
    ranges: Vec<CycleRange>,
}

/// Decode the planar body into one palette index per pixel (row-major).
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
            // At most 8 planes (checked in `parse`), so the index fits in a byte.
            indices[y * width + x] = index as u8;
        }
    }
    indices
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

/// Parse an in-memory ILBM/IFF image into its palette, per-pixel indices, and
/// colour-cycling ranges.
fn parse(bytes: &[u8]) -> Result<Parsed> {
    let form = Chunk { data: bytes };
    if form.id() != "FORM" || form.data().get(0..4) != Some(b"ILBM") {
        bail!("not an ILBM FORM");
    }

    let bmhd = form
        .find("BMHD")
        .ok_or_else(|| anyhow!("missing BMHD chunk"))?;
    let header = BmHeader::parse(bmhd.data())?;

    let camg = form
        .find("CAMG")
        .and_then(|c| c.data().get(0..4).map(|b| u32::from_be_bytes(b.try_into().unwrap())))
        .unwrap_or(0);
    let is_ham = camg & CAMG_HAM != 0;
    let is_ehb = camg & CAMG_EHB != 0;

    // Colour map: RGB triplets, one per palette entry.
    let mut palette: Vec<[u8; 3]> = form
        .find("CMAP")
        .map(|c| c.data().chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
        .unwrap_or_default();

    let body = form
        .find("BODY")
        .ok_or_else(|| anyhow!("missing BODY chunk"))?;

    let width = header.width as usize;
    let height = header.height as usize;
    let num_planes = header.num_planes as usize;
    if num_planes == 0 || num_planes > 8 {
        bail!("unsupported plane count: {num_planes}");
    }

    // Bytes per bitplane per scanline: rows are padded to a 16-bit boundary.
    let row_bytes = ((width + 15) / 16) * 2;
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

    let indices = decode_indices(width, height, num_planes, row_bytes, planes_per_row, &planar);

    // Extra-HalfBrite: expand the palette to 64 registers so a plain index
    // lookup yields the upper half-bright colours, letting EHB use the same
    // indexed path as any other palette image.
    if is_ehb {
        palette.resize(32, [0, 0, 0]);
        let base: Vec<[u8; 3]> = palette[..32].to_vec();
        palette.extend(base.iter().map(|c| [c[0] >> 1, c[1] >> 1, c[2] >> 1]));
    }

    let ranges = parse_ranges(&form);

    Ok(Parsed {
        width,
        height,
        num_planes,
        is_ham,
        palette,
        indices,
        ranges,
    })
}

/// Decode an in-memory ILBM/IFF image into an RGBA image (colours resolved,
/// cycling not applied).
pub fn load_from_memory(bytes: &[u8]) -> Result<RgbaImage> {
    let p = parse(bytes)?;
    let mut img = RgbaImage::new(p.width as u32, p.height as u32);
    for y in 0..p.height {
        // HAM carries colour forward across a scanline, seeded from black.
        let mut prev = [0u8, 0, 0];
        for x in 0..p.width {
            let index = p.indices[y * p.width + x] as usize;
            let rgb = if p.is_ham {
                ham_pixel(index, p.num_planes, &p.palette, &mut prev)
            } else {
                p.palette.get(index).copied().unwrap_or([0, 0, 0])
            };
            img.put_pixel(x as u32, y as u32, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }
    Ok(img)
}

/// Decode an in-memory ILBM/IFF image into an [`IndexedImage`], preserving the
/// palette and per-pixel indices so colour cycling can be applied at display
/// time. Fails for HAM images, whose pixels aren't plain palette lookups.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    let p = parse(bytes)?;
    if p.is_ham {
        bail!("HAM images can't be displayed as an indexed (colour-cycled) image");
    }
    Ok(IndexedImage {
        width: p.width as u32,
        height: p.height as u32,
        palette: p.palette,
        indices: p.indices,
        ranges: p.ranges,
    })
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
pub fn load(path: &Path) -> Result<RgbaImage> {
    load_from_memory(&fs::read(path)?)
}

/// Load an ILBM/IFF image from a file into an [`IndexedImage`] (see
/// [`load_indexed_from_memory`]).
pub fn load_indexed(path: &Path) -> Result<IndexedImage> {
    load_indexed_from_memory(&fs::read(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ilbm() {
        let img = load(Path::new("test.iff")).unwrap();
        assert_eq!(img.dimensions(), (640, 512));
        // The image must not be entirely one colour.
        let first = img.get_pixel(0, 0);
        assert!(img.pixels().any(|p| p != first), "decoded image is blank");
        img.save("test_ilbm_out.png").unwrap();
    }
}
