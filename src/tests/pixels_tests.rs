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

/// The documented reference points: a flat screen has no spread, a black/white
/// checkerboard has the maximum 0.5, and a full black-to-white flip is a frame
/// diff of 1.0.
#[test]
fn frame_stats_hit_their_reference_values() {
    let white = u32::from_ne_bytes([255, 255, 255, 255]);
    let black = u32::from_ne_bytes([0, 0, 0, 255]);

    let flat = vec![white; 64];
    let stats = get_frame_stats(&flat, &[], 0.0);
    assert_eq!(stats.avg_color, white);
    assert!(stats.color_diff < 0.001, "{}", stats.color_diff);
    assert_eq!(stats.frame_diff, 0.0);

    let checker: Vec<u32> = (0..64)
        .map(|i| if i % 2 == 0 { white } else { black })
        .collect();
    let stats = get_frame_stats(&checker, &flat, 0.0);
    assert!(
        (stats.color_diff - 0.5).abs() < 0.002,
        "{}",
        stats.color_diff
    );
    assert!(
        (stats.frame_diff - 0.5).abs() < 0.002,
        "{}",
        stats.frame_diff
    );

    let stats = get_frame_stats(&vec![white; 64], &vec![black; 64], 0.0);
    assert!(
        (stats.frame_diff - 1.0).abs() < 0.001,
        "{}",
        stats.frame_diff
    );
    // One frame of a moving average, from a standing start.
    assert!((stats.aggregated_diff - AGG_ALPHA).abs() < 0.001);
}

/// A moving starfield has to read as "something is happening" just as much as a
/// screen-wide flash does, and an unchanged screen has to decay.
#[test]
fn aggregated_diff_ignores_how_much_moved() {
    let white = u32::from_ne_bytes([255, 255, 255, 255]);
    let black = u32::from_ne_bytes([0, 0, 0, 255]);
    let count = 320 * 240;

    let dark = vec![black; count];
    let mut stars = dark.clone();
    for i in 0..200 {
        stars[i * 371 % count] = white;
    }

    let starfield = get_frame_stats(&stars, &dark, 0.0).aggregated_diff;
    let flash = get_frame_stats(&vec![white; count], &dark, 0.0).aggregated_diff;
    assert!(starfield > flash * 0.9, "{starfield} vs {flash}");

    let still = get_frame_stats(&stars, &stars, flash).aggregated_diff;
    assert!(still < flash, "{still} vs {flash}");
}

/// Micro-benchmark of the per-frame statistics, which run on the emulator
/// worker thread for every frame. Run:
///   cargo test --profile release-fast frame_stats_bench -- --ignored --nocapture
#[test]
#[ignore]
fn frame_stats_bench() {
    use std::hint::black_box;
    use std::time::Instant;

    for (width, height) in [(320usize, 240usize), (640, 480), (1920, 1080)] {
        let count = width * height;
        // A picture with real spread, and a next frame where a few percent of
        // the pixels moved - the shape the aggregate is tuned for.
        let a: Vec<u32> = (0..count)
            .map(|i| u32::from_ne_bytes([(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 255]))
            .collect();
        let mut b = a.clone();
        for i in (0..count).step_by(37) {
            b[i] ^= 0x00ff_ffff;
        }

        let iters = (60_000_000 / count).max(20);
        let bench = |name: &str, mut f: Box<dyn FnMut(&[u32], &[u32], f32) -> f32>| {
            let mut agg = 0.0f32;
            for _ in 0..iters / 10 {
                agg = f(black_box(&a), black_box(&b), agg);
            }
            let start = Instant::now();
            for _ in 0..iters {
                agg = f(black_box(&a), black_box(&b), agg);
            }
            let us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;
            eprintln!("  {width:>4}x{height:<4} {name:<16} {us:8.1} us/frame");
        };
        bench(
            "get_frame_stats",
            Box::new(|a, b, agg| get_frame_stats(a, b, agg).aggregated_diff),
        );
        bench(
            "get_frame_diff",
            Box::new(|a, b, agg| get_frame_diff(a, b, agg).1),
        );
        bench(
            "diff (scalar)",
            Box::new(|a, b, agg| {
                black_box(diff_pixels_scalar(a, b, 1));
                agg
            }),
        );
        bench(
            "diff (simd)",
            Box::new(|a, b, agg| {
                black_box(diff_pixels(a, b, 1));
                agg
            }),
        );
        bench(
            &format!("diff (skip {})", sample_skip(count)),
            Box::new(move |a, b, agg| {
                black_box(diff_pixels(a, b, sample_skip(a.len())));
                agg
            }),
        );
    }
}

/// The vector kernels have to agree with the scalar one exactly, at every
/// skip and including on a pixel count that leaves a partial chunk.
#[test]
fn diff_pixels_matches_scalar() {
    for count in [0usize, 1, 3, 8, 15, 16, 17, 63, 1000] {
        let a: Vec<u32> = (0..count)
            .map(|i| u32::from_ne_bytes([(i * 31) as u8, (i * 7) as u8, (i * 113) as u8, 255]))
            .collect();
        let b: Vec<u32> = (0..count)
            .map(|i| u32::from_ne_bytes([(i * 29) as u8, (i * 7 + 8) as u8, (i * 113) as u8, 0]))
            .collect();
        for skip in [1usize, 3, 7] {
            let want = diff_pixels_scalar(&a, &b, skip);
            assert_eq!(diff_pixels(&a, &b, skip), want, "{count} skip {skip}");
        }
    }
}

/// Sampling only has to estimate the same numbers, so a skip must not move
/// either diff much on a picture that is uniform in the large.
#[test]
fn sampling_estimates_the_full_frame() {
    let count = 1920 * 1080;
    let a: Vec<u32> = (0..count)
        .map(|i| u32::from_ne_bytes([(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, 255]))
        .collect();
    let mut b = a.clone();
    for i in (0..count).step_by(37) {
        b[i] ^= 0x00ff_ffff;
    }

    let full = diff_pixels(&a, &b, 1);
    let skip = sample_skip(count);
    assert!(skip > 1, "1080p should be sampled, got {skip}");
    let part = diff_pixels(&a, &b, skip);

    let mean = |(changed, _, sampled): (u64, u64, usize)| changed as f64 / sampled as f64;
    let rate = |(_, moved, sampled): (u64, u64, usize)| moved as f64 / sampled as f64;
    assert!(
        (mean(part) / mean(full) - 1.0).abs() < 0.05,
        "{part:?} vs {full:?}"
    );
    assert!(
        (rate(part) / rate(full) - 1.0).abs() < 0.05,
        "{part:?} vs {full:?}"
    );
}
