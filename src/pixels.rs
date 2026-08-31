const fn expand5(c: u8) -> u8 {
    (c << 3) | (c >> 2)
}

const fn expand6(c: u8) -> u8 {
    (c << 2) | (c >> 4)
}

/// Precomputed RGB565 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`. Replaces the per-pixel bit unpacking in
/// [`RetroCoreDirect::video_refresh`].
pub static RGB565_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 11) & 0x1f) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand6(g6), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Precomputed 0RGB1555 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`.
pub static RGB1555_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 10) & 0x1f) as u8;
        let g5 = ((v >> 5) & 0x1f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand5(g5), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Convert a 16-bits-per-pixel libretro framebuffer to packed RGBA8888 using
/// `lut`, which maps each raw 16-bit little-endian pixel to one output pixel.
/// `dst` must already be sized to `width * height`.
pub fn convert_16bpp(
    src: &[u8],
    dst: &mut [u32],
    width: usize,
    height: usize,
    pitch: usize,
    lut: &[u32; 65536],
) {
    for y in 0..height {
        let src_row = &src[y * pitch..y * pitch + width * 2];
        let dst_row = &mut dst[y * width..(y + 1) * width];
        for (out, px) in dst_row.iter_mut().zip(src_row.chunks_exact(2)) {
            let p = u16::from_le_bytes([px[0], px[1]]) as usize;
            *out = lut[p];
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold `frame` (one packed RGBA8888 pixel per `u32`, as handed to
/// [`Backend::with_frame`]) into its hash and its uniform-colour flags in one
/// pass.
///
/// Pixels are hashed in pairs so the multiply is amortized over two of them; a
/// 320x240 frame is ~38k iterations of a handful of ALU ops, which is noise next
/// to the emulation that produced it. An empty frame reports as both black and
/// white — callers are expected to have a real frame in hand.
pub fn scan_frame(frame: &[u32]) -> u64 {
    let mut hash = FNV_OFFSET;

    let mut pairs = frame.chunks_exact(2);
    for p in &mut pairs {
        let w = (p[0] as u64) | ((p[1] as u64) << 32);
        hash = (hash ^ w).wrapping_mul(FNV_PRIME);
    }

    // An odd pixel count leaves one pixel over; hash it alone in the low half.
    if let [px] = *pairs.remainder() {
        hash = (hash ^ px as u64).wrapping_mul(FNV_PRIME);
    }
    hash
}
