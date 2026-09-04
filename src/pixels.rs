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

/// Convert an XRGB8888 libretro framebuffer to packed RGBA8888.
/// `dst` must already be sized to `width * height`.
///
/// The source is BGRA in memory (little-endian XRGB8888), so every pixel is a
/// pure byte permutation of its destination: as a `u32` it is `0xAARRGGBB` and
/// we want `0xAABBGGRR`, which is `swap_bytes()` followed by `rotate_right(8)`.
///
/// Spelling it that way rather than as `from_ne_bytes([px[2], px[1], px[0],
/// px[3]])` is the whole point: LLVM recognises bswap+rotate as a byte shuffle
/// and vectorizes it, while the byte-at-a-time version stays scalar at four
/// `movzbl`s, three shifts and three `or`s per pixel. Measured on a 320x240
/// frame: 37 us scalar, 11 us with baseline SSE2 (two shuffles plus a
/// shift/or per 4 pixels), 3.3 us with SSSE3 (one `pshufb` per 4 pixels),
/// and 2.4 us with AVX2 (one `vpshufb` per 8).
#[inline(always)]
fn convert_xrgb8888_impl(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    for y in 0..height {
        let src_row = &src[y * pitch..y * pitch + width * 4];
        let dst_row = &mut dst[y * width..(y + 1) * width];
        // Indexed rather than `iter_mut().zip(chunks_exact(4))` on purpose: the
        // `Zip::new` call does not get inlined without LTO, and an unvectorized
        // loop body is the entire cost here.
        for x in 0..width {
            let v = u32::from_ne_bytes([
                src_row[x * 4],
                src_row[x * 4 + 1],
                src_row[x * 4 + 2],
                src_row[x * 4 + 3],
            ]);
            dst_row[x] = v.swap_bytes().rotate_right(8);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
fn convert_xrgb8888_avx2(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "ssse3")]
fn convert_xrgb8888_ssse3(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

/// See [`convert_xrgb8888_impl`]. We ship a baseline x86-64 binary, so the
/// `pshufb` that makes this a single instruction per 4 pixels is only reachable
/// through runtime dispatch. `is_x86_feature_detected!` caches its answer in an
/// atomic, so the per-frame cost is a relaxed load.
pub fn convert_xrgb8888(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { convert_xrgb8888_avx2(src, dst, width, height, pitch) };
        }
        if is_x86_feature_detected!("ssse3") {
            return unsafe { convert_xrgb8888_ssse3(src, dst, width, height, pitch) };
        }
    }
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

#[cfg(test)]
#[path = "tests/pixels_tests.rs"]
mod tests;
