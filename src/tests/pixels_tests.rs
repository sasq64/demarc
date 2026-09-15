use super::*;

/// Every dispatch target must agree with a byte-for-byte reference, including
/// on a width that leaves a partial SIMD vector and a pitch with padding.
#[test]
fn xrgb8888_repacks_bgra_to_rgba() {
    for (width, height) in [(1usize, 1usize), (7, 3), (320, 8)] {
        let pitch = width * 4 + 12;
        let src: Vec<u8> = (0..pitch * height).map(|i| (i * 37 % 251) as u8).collect();

        let mut expected = vec![0u32; width * height];
        for y in 0..height {
            for x in 0..width {
                let px = &src[y * pitch + x * 4..][..4];
                expected[y * width + x] = u32::from_ne_bytes([px[2], px[1], px[0], px[3]]);
            }
        }

        let mut dst = vec![0u32; width * height];
        convert_xrgb8888(&src, &mut dst, width, height, pitch);
        assert_eq!(dst, expected, "dispatched, {width}x{height}");

        dst.fill(0);
        convert_xrgb8888_impl(&src, &mut dst, width, height, pitch);
        assert_eq!(dst, expected, "portable, {width}x{height}");
    }
}
