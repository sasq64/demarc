use super::*;

/// A TIFF built field by field, so a test can state exactly which tags a file
/// carries and what they say. Values are laid out after the directory and
/// referred to by offset, except where they fit in an entry's four value bytes
/// — the same choice a real writer makes, and the one the decoder has to
/// follow.
struct Builder {
    order: Order,
    /// Tag, field type and the values, kept sorted: TIFF requires a directory
    /// in ascending tag order and this is where that is enforced.
    fields: Vec<(u16, u16, Vec<u32>)>,
    /// Pixel data, which the strip offsets point into. It goes first so the
    /// offsets are known before the directory is laid out.
    strips: Vec<Vec<u8>>,
}

/// TIFF field types, by their numbers in the specification.
const BYTE: u16 = 1;
const SHORT: u16 = 3;
const LONG: u16 = 4;

impl Builder {
    /// A `width` x `height` palette image of `bits`-deep pixels, with a
    /// greyscale ramp for a palette and every row in one strip.
    fn new(width: u32, height: u32, bits: u32, pixels: &[u8]) -> Self {
        let row_bytes = (width as usize * bits as usize).div_ceil(8);
        let strip: Vec<u8> = pack_rows(pixels, width as usize, height as usize, bits as usize);
        assert_eq!(strip.len(), row_bytes * height as usize);
        let colors = 1usize << bits;
        // A ramp over the depth's range, in the 16-bit form the tag specifies.
        let ramp = |i: usize| (i * 65535 / (colors - 1).max(1)) as u32;
        let mut map = Vec::new();
        for _ in 0..3 {
            map.extend((0..colors).map(ramp));
        }
        let mut builder = Builder {
            order: Order::Little,
            fields: Vec::new(),
            strips: vec![strip],
        };
        builder
            .tag(TAG_IMAGE_WIDTH, SHORT, &[width])
            .tag(TAG_IMAGE_LENGTH, SHORT, &[height])
            .tag(TAG_BITS_PER_SAMPLE, SHORT, &[bits])
            .tag(TAG_COMPRESSION, SHORT, &[COMPRESSION_NONE])
            .tag(TAG_PHOTOMETRIC, SHORT, &[PHOTOMETRIC_RGB_PALETTE])
            .tag(TAG_SAMPLES_PER_PIXEL, SHORT, &[1])
            .tag(TAG_ROWS_PER_STRIP, SHORT, &[height])
            .tag(TAG_COLOR_MAP, SHORT, &map);
        builder
    }

    /// Set a tag, replacing any value it already had.
    fn tag(&mut self, tag: u16, kind: u16, values: &[u32]) -> &mut Self {
        self.fields.retain(|(t, _, _)| *t != tag);
        self.fields.push((tag, kind, values.to_vec()));
        self
    }

    fn remove(&mut self, tag: u16) -> &mut Self {
        self.fields.retain(|(t, _, _)| *t != tag);
        self
    }

    fn byte_order(&mut self, order: Order) -> &mut Self {
        self.order = order;
        self
    }

    /// Replace the pixel data with strips of already-encoded bytes, as a
    /// compressed or multi-strip file carries them.
    fn packed(&mut self, strips: Vec<Vec<u8>>) -> &mut Self {
        self.strips = strips;
        self
    }

    fn u16(&self, v: u16) -> [u8; 2] {
        match self.order {
            Order::Little => v.to_le_bytes(),
            Order::Big => v.to_be_bytes(),
        }
    }

    fn u32(&self, v: u32) -> [u8; 4] {
        match self.order {
            Order::Little => v.to_le_bytes(),
            Order::Big => v.to_be_bytes(),
        }
    }

    /// Serialise: header, strips, directory, then the values too big to sit
    /// inside their entries.
    fn build(&self) -> Vec<u8> {
        let mut fields = self.fields.clone();
        fields.sort_by_key(|(tag, _, _)| *tag);

        let mut out = match self.order {
            Order::Little => b"II\x2a\x00".to_vec(),
            Order::Big => b"MM\x00\x2a".to_vec(),
        };
        // The offset of the directory, filled in once the strips are placed.
        out.extend([0; 4]);

        let mut offsets = Vec::new();
        let mut counts = Vec::new();
        for strip in &self.strips {
            offsets.push(out.len() as u32);
            counts.push(strip.len() as u32);
            out.extend(strip);
        }
        fields.retain(|(tag, _, _)| *tag != TAG_STRIP_OFFSETS && *tag != TAG_STRIP_BYTE_COUNTS);
        fields.push((TAG_STRIP_OFFSETS, LONG, offsets));
        fields.push((TAG_STRIP_BYTE_COUNTS, LONG, counts));
        fields.sort_by_key(|(tag, _, _)| *tag);

        let directory = out.len() as u32;
        let ifd_bytes = out.len() + 2 + fields.len() * 12 + 4;
        out[4..8].copy_from_slice(&self.u32(directory));
        out.extend(self.u16(fields.len() as u16));

        // Values that don't fit in an entry go after the directory; `spilled`
        // collects them as the entries are written.
        let mut spilled: Vec<u8> = Vec::new();
        for (tag, kind, values) in &fields {
            let size = field_size(*kind).unwrap();
            let mut encoded = Vec::new();
            for &value in values {
                match size {
                    1 => encoded.push(value as u8),
                    2 => encoded.extend(self.u16(value as u16)),
                    _ => encoded.extend(self.u32(value)),
                }
            }
            out.extend(self.u16(*tag));
            out.extend(self.u16(*kind));
            out.extend(self.u32(values.len() as u32));
            if encoded.len() <= 4 {
                encoded.resize(4, 0);
                out.extend(encoded);
            } else {
                out.extend(self.u32((ifd_bytes + spilled.len()) as u32));
                spilled.extend(encoded);
            }
        }
        // No second directory.
        out.extend([0; 4]);
        out.extend(spilled);
        out
    }
}

/// Pack one index per pixel into rows of `bits`-deep samples, each row padded
/// out to a whole number of bytes — the layout [`unpack_samples`] undoes.
fn pack_rows(pixels: &[u8], width: usize, height: usize, bits: usize) -> Vec<u8> {
    let row_bytes = (width * bits).div_ceil(8);
    let mut out = vec![0u8; row_bytes * height];
    for y in 0..height {
        for x in 0..width {
            let bit = x * bits;
            let shift = 8 - bits - (bit % 8);
            out[y * row_bytes + bit / 8] |= pixels[y * width + x] << shift;
        }
    }
    out
}

/// The palette `Builder::new` writes at `bits` deep: a ramp that reaches full
/// white at the top index, which for 8 bits is the identity.
fn ramp(bits: u32) -> Vec<[u8; 3]> {
    let colors = 1usize << bits;
    (0..colors)
        .map(|i| {
            let v = ((i * 65535 / (colors - 1).max(1)) >> 8) as u8;
            [v, v, v]
        })
        .collect()
}

/// A small picture using every one of eight indices.
fn pixels_8bit() -> Vec<u8> {
    (0..12).map(|i| (i * 3 % 8) as u8).collect()
}

#[test]
fn decodes_an_uncompressed_palette_image() {
    let pixels = pixels_8bit();
    let tiff = Builder::new(4, 3, 8, &pixels).build();
    let img = load_indexed_from_memory(&tiff).unwrap();

    assert_eq!((img.width, img.height), (4, 3));
    assert_eq!(img.indices, pixels);
    // The palette survives as a palette — the whole point of decoding here
    // rather than letting the picture arrive as truecolour.
    assert_eq!(img.palette, ramp(8));
    // Nothing in a TIFF cycles.
    assert!(img.ranges.is_empty());
}

#[test]
fn reads_either_byte_order() {
    let pixels = pixels_8bit();
    let little = Builder::new(4, 3, 8, &pixels).build();
    let big = Builder::new(4, 3, 8, &pixels)
        .byte_order(Order::Big)
        .build();
    assert_ne!(little, big);

    let decoded = load_indexed_from_memory(&big).unwrap();
    assert_eq!(
        decoded.indices,
        load_indexed_from_memory(&little).unwrap().indices
    );
    assert_eq!(decoded.palette, ramp(8));
}

#[test]
fn reads_tags_of_any_integer_field_type() {
    // The same field may be written as a byte, a short or a long, and real
    // writers mix them freely — dimensions in particular are usually longs.
    let pixels = pixels_8bit();
    let tiff = Builder::new(4, 3, 8, &pixels)
        .tag(TAG_IMAGE_WIDTH, LONG, &[4])
        .tag(TAG_IMAGE_LENGTH, LONG, &[3])
        .tag(TAG_SAMPLES_PER_PIXEL, BYTE, &[1])
        .build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!((img.width, img.height), (4, 3));
    assert_eq!(img.indices, pixels);
}

#[test]
fn unpacks_sub_byte_depths_over_padded_rows() {
    // Five pixels of four bits each is two and a half bytes, so every row is
    // padded with half a byte the decoder has to step over.
    let pixels: Vec<u8> = (0..15).map(|i| (i * 7 % 16) as u8).collect();
    let tiff = Builder::new(5, 3, 4, &pixels).build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!(img.indices, pixels);
    assert_eq!(img.palette.len(), 16);
    assert_eq!(img.palette[15], [255, 255, 255]);

    // The same at one bit, where a row of five pixels wastes three.
    let bits: Vec<u8> = (0..15).map(|i| (i % 2) as u8).collect();
    let tiff = Builder::new(5, 3, 1, &bits).build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!(img.indices, bits);
    assert_eq!(img.palette, vec![[0, 0, 0], [255, 255, 255]]);
}

#[test]
fn colour_maps_written_as_bytes_are_taken_at_face_value() {
    // The tag is specified as 16-bit channels, but writers that put an 8-bit
    // value in the field are common enough that reading them the specified way
    // — as very dark 16-bit colours — would be the wrong answer.
    let map: Vec<u32> = (0..3)
        .flat_map(|c| (0..16).map(move |i| i * 16 + c))
        .collect();
    let tiff = Builder::new(4, 4, 4, &[1; 16])
        .tag(TAG_COLOR_MAP, SHORT, &map)
        .build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!(img.palette[1], [16, 17, 18]);
    assert_eq!(img.palette[15], [240, 241, 242]);
}

#[test]
fn strips_are_stitched_back_together() {
    let pixels: Vec<u8> = (0..28).map(|i| (i * 5 % 256) as u8).collect();
    let rows_per_strip = 3usize;
    // Four bytes a row, three rows a strip — and a last strip holding the one
    // row left over, which is the case an even split would never exercise.
    let strips: Vec<Vec<u8>> = pixels
        .chunks(4 * rows_per_strip)
        .map(<[u8]>::to_vec)
        .collect();
    assert_eq!(strips.len(), 3);
    assert_eq!(strips[2].len(), 4);

    let tiff = Builder::new(4, 7, 8, &pixels)
        .tag(TAG_ROWS_PER_STRIP, SHORT, &[rows_per_strip as u32])
        .packed(strips)
        .build();
    assert_eq!(load_indexed_from_memory(&tiff).unwrap().indices, pixels);
}

#[test]
fn packbits_strips_are_run_length_decoded() {
    // Two rows of eight pixels, each written as PackBits' two operations: a
    // count of -3 repeating the byte that follows four times, then a literal
    // run of four bytes.
    let row = [0xfdu8, 0x07, 0x03, 0x01, 0x02, 0x03, 0x04];
    let tiff = Builder::new(8, 2, 8, &[0; 16])
        .tag(TAG_COMPRESSION, SHORT, &[COMPRESSION_PACKBITS])
        .tag(TAG_ROWS_PER_STRIP, SHORT, &[2])
        .packed(vec![[row, row].concat()])
        .build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!(img.indices, [7, 7, 7, 7, 1, 2, 3, 4].repeat(2));
}

#[test]
fn horizontal_differencing_is_undone() {
    // Each byte stored as its difference from the one to its left, so a row of
    // ones decodes to a ramp — and the wrap is part of the coding, not an
    // overflow.
    let tiff = Builder::new(4, 2, 8, &[0; 8])
        .tag(TAG_PREDICTOR, SHORT, &[PREDICTOR_HORIZONTAL])
        .packed(vec![vec![250, 1, 1, 10, 1, 1, 1, 1]])
        .build();
    let img = load_indexed_from_memory(&tiff).unwrap();
    assert_eq!(img.indices, [250, 251, 252, 6, 1, 2, 3, 4]);

    // It only lines up with bytes at eight bits a sample.
    let tiff = Builder::new(4, 2, 4, &[0; 8])
        .tag(TAG_PREDICTOR, SHORT, &[PREDICTOR_HORIZONTAL])
        .build();
    assert!(load_indexed_from_memory(&tiff).is_err());
}

#[test]
fn lzw_matches_a_reference_encoder() {
    // `LZW_STRIP` is what Pillow wrote for `reference_pixels`, so this checks
    // the decoder against a real encoder rather than against itself — and the
    // data is long enough that the code width grows past nine bits, which is
    // where a decoder that misses TIFF's early change starts reading garbage.
    let pixels = reference_pixels();
    assert_eq!(lzw_decode(&LZW_STRIP, pixels.len()).unwrap(), pixels);

    let tiff = Builder::new(64, 8, 8, &pixels)
        .tag(TAG_COMPRESSION, SHORT, &[COMPRESSION_LZW])
        .packed(vec![LZW_STRIP.to_vec()])
        .build();
    assert_eq!(load_indexed_from_memory(&tiff).unwrap().indices, pixels);
}

#[test]
fn leaves_everything_else_to_the_image_crate() {
    let pixels = pixels_8bit();
    // A greyscale or truecolour TIFF is not ours to decode.
    let tiff = Builder::new(4, 3, 8, &pixels)
        .tag(TAG_PHOTOMETRIC, SHORT, &[1])
        .build();
    assert!(load_indexed_from_memory(&tiff).is_err());
    // Nor is a tiled one, or one with no palette to read.
    let tiff = Builder::new(4, 3, 8, &pixels)
        .tag(TAG_TILE_WIDTH, SHORT, &[16])
        .build();
    assert!(load_indexed_from_memory(&tiff).is_err());
    let tiff = Builder::new(4, 3, 8, &pixels).remove(TAG_COLOR_MAP).build();
    assert!(load_indexed_from_memory(&tiff).is_err());
    // A compression this decoder doesn't implement is an error naming it, not
    // a wrong picture.
    let tiff = Builder::new(4, 3, 8, &pixels)
        .tag(TAG_COMPRESSION, SHORT, &[7])
        .build();
    let message = load_indexed_from_memory(&tiff).err().unwrap().to_string();
    assert!(message.contains('7'), "{message}");
}

#[test]
fn refuses_files_that_are_not_a_baseline_tiff() {
    assert!(load_indexed_from_memory(b"").is_err());
    assert!(load_indexed_from_memory(b"FORM\0\0\0\x08ILBM").is_err());

    let tiff = Builder::new(4, 3, 8, &pixels_8bit()).build();
    // BigTIFF says 43 where a baseline file says 42.
    let mut big = tiff.clone();
    big[2] = 43;
    assert!(load_indexed_from_memory(&big).is_err());
    // A file cut short reports where it ran out instead of panicking.
    for len in 0..tiff.len() {
        assert!(
            load_indexed_from_memory(&tiff[..len]).is_err(),
            "{len} bytes decoded"
        );
    }
}

#[test]
fn describes_the_picture() {
    let tiff = Builder::new(320, 200, 8, &[0; 320 * 200]).build();
    assert_eq!(describe(&tiff), "TIFF 320x200 (256 colors)");
    let tiff = Builder::new(16, 16, 4, &[0; 256]).build();
    assert_eq!(describe(&tiff), "TIFF 16x16 (16 colors)");
    // Best effort, like the other decoders' descriptions.
    assert_eq!(describe(b"not a tiff"), "TIFF");
}

/// The picture `LZW_STRIP` holds: runs and short novel sequences in the
/// proportions that make an LZW table grow.
fn reference_pixels() -> Vec<u8> {
    (0..64 * 8)
        .map(|i: usize| match (i / 5) % 3 {
            0 => ((i / 11) % 251) as u8,
            _ => ((i * 7 / 5) % 37) as u8,
        })
        .collect()
}

/// `reference_pixels` as Pillow's TIFF LZW encoder writes it (one strip of a
/// 64x8 8-bit palette image).
const LZW_STRIP: [u8; 364] = [
    0x80, 0x00, 0x20, 0x50, 0x20, 0x38, 0x20, 0x12, 0x0b, 0x06, 0x03, 0x81, 0xe1, 0x00, 0x90, 0x4c,
    0x03, 0x0f, 0x87, 0x87, 0x03, 0xa1, 0xe1, 0x00, 0x84, 0x46, 0x24, 0x00, 0x00, 0x80, 0x60, 0x28,
    0xe0, 0x0c, 0x06, 0x0c, 0x06, 0x83, 0x82, 0x01, 0x10, 0x98, 0x50, 0x2a, 0x17, 0x0c, 0x01, 0x25,
    0x52, 0xa1, 0x08, 0x88, 0x46, 0x00, 0x00, 0x80, 0xc0, 0x80, 0x58, 0x28, 0x16, 0x6d, 0x36, 0x08,
    0xc3, 0x42, 0xa1, 0x60, 0xc0, 0x64, 0x35, 0x12, 0x03, 0x01, 0x80, 0xf4, 0x30, 0x0c, 0x68, 0x0a,
    0x06, 0x83, 0x02, 0xa4, 0x00, 0x8a, 0x65, 0x30, 0x2d, 0x28, 0x0d, 0x06, 0xe2, 0x61, 0xf9, 0x68,
    0x26, 0xad, 0x56, 0xa1, 0x02, 0x01, 0x40, 0xb9, 0x08, 0x3e, 0x73, 0x5b, 0x05, 0xd8, 0x43, 0x71,
    0x20, 0xf8, 0x82, 0x5c, 0x24, 0xa2, 0x83, 0x2d, 0x56, 0xa8, 0x40, 0x36, 0x17, 0x0d, 0x0a, 0x53,
    0xc1, 0xb7, 0x39, 0x0c, 0x54, 0x45, 0x18, 0x8d, 0x01, 0x28, 0x40, 0xfb, 0xe5, 0xf2, 0x46, 0x12,
    0x93, 0x4a, 0x03, 0x36, 0x30, 0x86, 0x17, 0x0b, 0x30, 0x01, 0x4c, 0xe0, 0xb0, 0x70, 0x60, 0x47,
    0x1c, 0x12, 0x09, 0x4e, 0xc2, 0xf3, 0xe8, 0x94, 0x50, 0x42, 0x13, 0xcc, 0x66, 0x28, 0xe0, 0x70,
    0x4d, 0x2a, 0x43, 0x23, 0x0a, 0x68, 0x74, 0x35, 0x10, 0xe0, 0x7a, 0xa9, 0x2e, 0x98, 0x05, 0x67,
    0x61, 0x60, 0xb5, 0x6e, 0x13, 0x5e, 0x9d, 0x53, 0xc2, 0xfb, 0x30, 0xbd, 0x96, 0x2d, 0x68, 0xa3,
    0x01, 0x83, 0x1b, 0xbd, 0xdc, 0x2e, 0x49, 0x71, 0xa8, 0x06, 0xe7, 0xc1, 0xae, 0x24, 0x62, 0x63,
    0x7a, 0x82, 0xd6, 0xc3, 0x7c, 0xbe, 0x5c, 0x9a, 0x79, 0x83, 0xb2, 0x08, 0x03, 0x9d, 0x38, 0x94,
    0xce, 0x91, 0x8c, 0xb7, 0x04, 0x03, 0xdd, 0xbe, 0xdc, 0xfa, 0xa5, 0x96, 0xbb, 0x80, 0x03, 0xfe,
    0x3f, 0x1e, 0x76, 0xb9, 0x22, 0x9c, 0xc9, 0x84, 0x1e, 0xb1, 0x08, 0x87, 0x4d, 0x66, 0x97, 0xd1,
    0x66, 0x62, 0x2f, 0xa7, 0xd2, 0x15, 0x0c, 0x09, 0xe4, 0xa7, 0xc2, 0x3f, 0xe7, 0xf6, 0xf0, 0x01,
    0xb3, 0x6c, 0xe8, 0x48, 0x12, 0x04, 0xb0, 0x2a, 0x4a, 0x93, 0x83, 0x0d, 0x23, 0x4c, 0x13, 0x41,
    0x90, 0x62, 0x64, 0x9a, 0x2b, 0x48, 0x42, 0x14, 0x13, 0xc2, 0x90, 0xa2, 0x7a, 0x9f, 0x83, 0xad,
    0xb2, 0x2e, 0x14, 0x05, 0x30, 0xe8, 0x52, 0xa4, 0xa4, 0x0d, 0xf2, 0x4a, 0x15, 0x44, 0x91, 0x22,
    0xa6, 0x96, 0xb8, 0xc9, 0x90, 0x57, 0x15, 0x85, 0x61, 0x62, 0xba, 0xf4, 0xa7, 0x69, 0xe8, 0x5b,
    0x19, 0xc6, 0x6b, 0x3b, 0xe4, 0xa3, 0xa0, 0xc1, 0x70, 0x5c, 0x80, 0x80,
];
