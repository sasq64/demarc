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

/// Decode an in-memory ILBM/IFF image into an RGBA image.
pub fn load_from_memory(bytes: &[u8]) -> Result<RgbaImage> {
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
    let palette: Vec<[u8; 3]> = form
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

    let mut img = RgbaImage::new(header.width as u32, header.height as u32);

    for y in 0..height {
        let row_base = y * planes_per_row * row_bytes;
        // HAM carries colour forward across a scanline, seeded from black.
        let mut prev = [0u8, 0, 0];
        for x in 0..width {
            // Reassemble the pixel's plane bits into an index.
            let byte_idx = x / 8;
            let bit = 7 - (x % 8);
            let mut index = 0usize;
            for p in 0..num_planes {
                let b = planar[row_base + p * row_bytes + byte_idx];
                index |= (((b >> bit) & 1) as usize) << p;
            }

            let rgb = if is_ham {
                ham_pixel(index, num_planes, &palette, &mut prev)
            } else if is_ehb {
                ehb_pixel(index, &palette)
            } else {
                palette.get(index).copied().unwrap_or([0, 0, 0])
            };

            img.put_pixel(x as u32, y as u32, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
        }
    }

    Ok(img)
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

/// Resolve one Extra-HalfBrite pixel: indices >= 32 are half-bright copies.
fn ehb_pixel(index: usize, palette: &[[u8; 3]]) -> [u8; 3] {
    if index < 32 {
        palette.get(index).copied().unwrap_or([0, 0, 0])
    } else {
        let base = palette.get(index - 32).copied().unwrap_or([0, 0, 0]);
        [base[0] >> 1, base[1] >> 1, base[2] >> 1]
    }
}

/// Load an ILBM/IFF image from a file into an RGBA image.
pub fn load(path: &Path) -> Result<RgbaImage> {
    load_from_memory(&fs::read(path)?)
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
