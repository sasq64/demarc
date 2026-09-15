use super::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Per-stage micro-benchmark of the worker's hot loop. Drives the Ruffle
/// player stages directly (no channel / worker thread) so each stage's cost
/// is measured in isolation. Requires a GPU adapter, so `#[ignore]`d. Run:
///   FLASH_TEST_SWF=/path/to/movie.swf cargo test --release flash_bench -- --ignored --nocapture
#[test]
#[ignore]
fn flash_bench() {
    let path = std::env::var("FLASH_TEST_SWF")
        .expect("set FLASH_TEST_SWF to a .swf path to run this test");
    let frames: usize = std::env::var("FLASH_BENCH_FRAMES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);
    // Isolate ActionScript bytecode interpretation: run only `tick` (timeline
    // + AVM), skipping render/capture/audio so a profiler attributes ~all
    // samples to the VM. Set FLASH_BENCH_TICK_ONLY=1.
    let tick_only = std::env::var("FLASH_BENCH_TICK_ONLY").is_ok();
    // Some SWFs are preloaders/menus that only start heavy content after a
    // click (e.g. 99er.swf's "PLAY" button). FLASH_BENCH_CLICK="x,y" (0..1
    // normalized stage coords) injects a click at frame FLASH_BENCH_CLICK_AT
    // (default 60) so the benchmark exercises the real content.
    let click_xy: Option<(f64, f64)> = std::env::var("FLASH_BENCH_CLICK").ok().and_then(|s| {
        let (a, b) = s.split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    });
    let click_at: usize = std::env::var("FLASH_BENCH_CLICK_AT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);

    let (player, proxy, mut executor, descriptors, width, height, fps) =
        build_player(Path::new(&path)).expect("build player");
    eprintln!("movie: {width}x{height} @ {fps} fps, benching {frames} frames");

    #[derive(Default)]
    struct Stat {
        total: Duration,
        max: Duration,
    }
    impl Stat {
        fn add(&mut self, d: Duration) {
            self.total += d;
            if d > self.max {
                self.max = d;
            }
        }
        fn report(&self, name: &str, n: usize) {
            let avg = self.total.as_secs_f64() * 1e3 / n as f64;
            let max = self.max.as_secs_f64() * 1e3;
            eprintln!("  {name:<14} avg {avg:7.3} ms   max {max:7.3} ms");
        }
    }

    let (mut s_tick, mut s_render, mut s_exec, mut s_capture, mut s_mix) = (
        Stat::default(),
        Stat::default(),
        Stat::default(),
        Stat::default(),
        Stat::default(),
    );
    let mut audio_carry = 0.0f64;
    // Per-frame tick durations (ms) so we can show the amortization curve and
    // distinguish one-time load/preload cost from steady per-frame work.
    let mut tick_ms: Vec<f64> = Vec::with_capacity(frames);
    let loop_start = Instant::now();

    for frame_idx in 0..frames {
        if let Some((nx, ny)) = click_xy {
            if frame_idx == click_at {
                let (px, py) = (nx * width as f64, ny * height as f64);
                let mut p = player.lock().unwrap();
                p.handle_event(PlayerEvent::MouseMove { x: px, y: py });
                p.handle_event(PlayerEvent::MouseDown {
                    x: px,
                    y: py,
                    button: RuffleMouseButton::Left,
                    index: None,
                });
                p.handle_event(PlayerEvent::MouseUp {
                    x: px,
                    y: py,
                    button: RuffleMouseButton::Left,
                });
                eprintln!("injected click at ({px:.0},{py:.0}) on frame {frame_idx}");
            }
        }
        let live_fps = {
            let mut p = player.lock().unwrap();
            let frame_dur = FloatDuration::from_secs(1.0 / p.frame_rate().max(1.0));

            let t = Instant::now();
            p.tick(frame_dur);
            let dt = t.elapsed();
            tick_ms.push(dt.as_secs_f64() * 1e3);
            s_tick.add(dt);

            if !tick_only {
                let t = Instant::now();
                p.render();
                s_render.add(t.elapsed());
            }

            p.frame_rate().max(1.0)
        };

        // Pump external fetches (e.g. streamed `.mp3`/asset loads) every frame,
        // even in tick-only mode, so click-triggered content can stream in.
        let t = Instant::now();
        executor.run();
        s_exec.add(t.elapsed());

        if tick_only {
            continue;
        }

        let t = Instant::now();
        let mut frame = Vec::new();
        capture_frame_fast(&player, &descriptors, &mut frame);
        s_capture.add(t.elapsed());

        audio_carry += SAMPLE_RATE as f64 / live_fps;
        let n = audio_carry.floor() as usize;
        audio_carry -= n as f64;
        let mut audio = vec![0i16; n * 2];
        let t = Instant::now();
        proxy.mix::<i16>(&mut audio);
        s_mix.add(t.elapsed());
    }

    let wall = loop_start.elapsed();

    // Optional PNG of the final frame, to confirm what was actually running
    // (e.g. that a click started the real content).
    if let Ok(png) = std::env::var("FLASH_TEST_PNG") {
        let mut frame = Vec::new();
        if capture_frame_fast(&player, &descriptors, &mut frame) {
            if let Some(img) = image::RgbaImage::from_raw(width, height, frame) {
                let _ = img.save(&png);
                eprintln!("wrote {png} ({width}x{height})");
            }
        }
    }

    // Tick amortization: bucketed averages across the run, plus median. A
    // front-loaded curve (early buckets >> late) means one-time load/preload;
    // a flat curve means genuine per-frame work.
    {
        let buckets = 10usize.min(frames);
        let per = frames / buckets.max(1);
        eprint!("\ntick ms by phase ({per} frames each): ");
        for b in 0..buckets {
            let seg = &tick_ms[b * per..(b + 1) * per];
            let avg = seg.iter().sum::<f64>() / seg.len() as f64;
            eprint!("{avg:.2} ");
        }
        let mut sorted = tick_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        let p95 = sorted[sorted.len() * 95 / 100];
        eprintln!("\ntick median {median:.3} ms   p95 {p95:.3} ms   (avg is skewed by load)");
    }

    eprintln!("\nper-stage cost (avg/max over {frames} frames):");
    s_tick.report("tick", frames);
    s_render.report("render", frames);
    s_exec.report("executor.run", frames);
    s_capture.report("capture_frame", frames);
    s_mix.report("mix_audio", frames);
    let total_ms = wall.as_secs_f64() * 1e3;
    eprintln!(
        "\ntotal wall {total_ms:.1} ms  =>  {:.2} ms/frame  ({:.1} fps sustained, movie wants {fps} fps)",
        total_ms / frames as f64,
        frames as f64 / wall.as_secs_f64(),
    );
}

/// End-to-end smoke test: load an SWF, run a few frames, and confirm we get
/// a full RGBA frame at the movie's dimensions. Requires a working GPU
/// adapter, so it is `#[ignore]`d by default. Run with:
///   FLASH_TEST_SWF=/path/to/movie.swf cargo test flash_smoke -- --ignored --nocapture
#[test]
#[ignore]
fn flash_smoke() {
    let path = std::env::var("FLASH_TEST_SWF")
        .expect("set FLASH_TEST_SWF to a .swf path to run this test");
    let mut emu = FlashEmu::new(Path::new(&path), HashMap::new()).expect("load SWF");

    let (w, h) = emu.get_frame_size();
    assert!(w > 0 && h > 0, "movie has non-zero dimensions");
    assert!(emu.fps() > 0.0, "movie has a frame rate");

    // Pump until the worker delivers a rendered frame with real content.
    // (`run()` returns `true` unconditionally, like `RetroCoreThreaded`, so
    // detect the first frame by its pixels rather than the return value.)
    let mut got_frame = false;
    for _ in 0..600 {
        emu.run();
        emu.with_frame(&mut |_, _, buf| got_frame = buf.iter().any(|&px| px != 0));
        if got_frame {
            break;
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    assert!(got_frame, "worker produced a non-empty frame");

    // Let a few more frames accumulate so animated content is visible.
    for _ in 0..30 {
        emu.run();
        std::thread::sleep(Duration::from_millis(16));
    }

    emu.with_frame(&mut |fw, fh, buf| {
        assert_eq!(fw, w);
        assert_eq!(fh, h);
        assert_eq!(buf.len(), fw * fh, "RGBA8 buffer is w*h pixels");
        assert!(buf.iter().any(|&px| px != 0), "frame is not all-zero");
        // Confirm real rasterized content, not just a flat clear color.
        // Alpha is forced opaque, so compare the colour bytes only.
        let distinct = {
            let mut set = std::collections::HashSet::new();
            for &px in buf {
                let [r, g, b, _] = px.to_ne_bytes();
                set.insert([r, g, b]);
                if set.len() > 4 {
                    break;
                }
            }
            set.len()
        };
        assert!(distinct > 1, "frame has more than one color");

        if let Ok(png) = std::env::var("FLASH_TEST_PNG") {
            let img = image::RgbaImage::from_raw(
                fw as u32,
                fh as u32,
                crate::backend::frame_bytes(buf).to_vec(),
            )
            .expect("valid rgba buffer");
            img.save(&png).expect("save png");
            eprintln!("wrote {png} ({fw}x{fh})");
        }
    });
}
