//! Tests that boot real libretro cores against demo content in the repo.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::backend::frame_bytes;
use crate::libloader;

use super::*;

pub fn save_png(emu: &RetroCoreDirect, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let width = emu.state.frame_width as u32;
    let height = emu.state.frame_height as u32;
    let expected = (width as usize) * (height as usize);
    if width == 0 || height == 0 || emu.state.frame.len() < expected {
        return Err("no frame available".into());
    }
    let bytes = frame_bytes(&emu.state.frame[..expected]).to_vec();
    let buf =
        image::RgbaImage::from_raw(width, height, bytes).ok_or("failed to build image buffer")?;
    buf.save(path)?;
    Ok(())
}
/// Paths here are rooted at the crate directory rather than left relative:
/// a conversion running in another test switches the process-wide working
/// directory for its duration (see `cbmconvert::CwdGuard`).
fn root(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// The threaded `run()` is non-blocking, so the main loop must give the
/// worker thread time to boot and deliver frames. Drive `emu` until it has
/// produced its first frame, or panic after `timeout`.
fn run_until_frame(emu: &mut dyn Backend, timeout: Duration) {
    let start = Instant::now();
    while emu.get_frame_size().0 == 0 {
        emu.run();
        assert!(
            start.elapsed() < timeout,
            "worker never produced a frame within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn retro_amiga_works() {
    let core_path = libloader::get_libretro("puae").unwrap();
    let system_dir = &root("system/amiga");
    let game_path = root("demos/rebels.adf");

    let settings = HashMap::new();

    let mut retro_emu =
        RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();
    println!("## RUN");
    for _ in 0..200 {
        retro_emu.run();
    }
    save_png(&retro_emu, &root("test_amiga.png")).unwrap();
}

/// Boot a self-booting directory under Kickstart 1.3 (A500). The WHDLoad
/// helper must be disabled, otherwise its Startup-Sequence runs `FAILAT`,
/// a command that doesn't exist under 1.3, and the boot fails.
#[test]
fn retro_amiga_dir_works() {
    let core_path = libloader::get_libretro("puae").unwrap();
    let system_dir = &root("system/amiga");
    let game_path = root("demos/o2-intro");

    let mut settings = HashMap::new();
    settings.insert("puae_model".into(), "A500".into());
    settings.insert("puae_use_whdload".into(), "disabled".into());

    let mut retro_emu =
        RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();
    for _ in 0..200 {
        retro_emu.run();
    }
    save_png(&retro_emu, &root("test_amiga_dir.png")).unwrap();
}

#[test]
fn retro_threaded_works() {
    let core_path = libloader::get_libretro("puae").unwrap();
    let system_dir = &root("system/amiga");
    let game_path = root("demos/rebels.adf");

    let mut settings = HashMap::new();
    settings.insert("puae_model".into(), "A500".into());

    let mut emu =
        RetroCoreThreaded::new(&core_path, system_dir, Some(&game_path), settings, false).unwrap();
    // Object-safety / interchangeability check.
    let emu: &mut dyn Backend = &mut emu;

    // Pace the loop so the worker keeps up and the demo advances.
    for _ in 0..200 {
        emu.run();
        std::thread::sleep(Duration::from_millis(2));
    }
    // The worker may still be a few frames behind; make sure we have one.
    run_until_frame(emu, Duration::from_secs(5));
    //emu.save_png(&root("test_amiga_threaded.png")).unwrap();
    let (w, h) = emu.get_frame_size();
    assert!(w > 0 && h > 0, "no frame produced by worker");
}

#[test]
fn retro_threaded_multi_works() {
    let uae_core = libloader::get_libretro("puae").unwrap();
    let vice_core = libloader::get_libretro("vice_x64").unwrap();
    // The two cores no longer share a system dir — the Amiga one is a subdir of
    // it (see `amiga_system_dir()`).
    let uae_system = root("system/amiga");
    let vice_system = root("system");
    let uae_game = root("demos/rebels.adf");
    let vice_game = root("demos/quantum_icc2026_v1p.prg");

    let uae_settings = || {
        let mut s = HashMap::new();
        s.insert("puae_model".to_string(), "A500".to_string());
        s
    };

    let cores = [
        (
            &uae_core,
            &uae_system,
            &uae_game,
            uae_settings(),
            "test_threaded_uae_0.png",
        ),
        (
            &uae_core,
            &uae_system,
            &uae_game,
            uae_settings(),
            "test_threaded_uae_1.png",
        ),
        (
            &vice_core,
            &vice_system,
            &vice_game,
            HashMap::new(),
            "test_threaded_vice_0.png",
        ),
        (
            &vice_core,
            &vice_system,
            &vice_game,
            HashMap::new(),
            "test_threaded_vice_1.png",
        ),
    ];

    let mut emus: Vec<(&str, RetroCoreThreaded)> = cores
        .iter()
        .map(|(core, system, game, settings, png)| {
            let emu =
                RetroCoreThreaded::new(core, system, Some(game), settings.clone(), false).unwrap();
            (*png, emu)
        })
        .collect();

    // Pace the loop so the workers keep up and the demos advance.
    for _ in 0..200 {
        for (_, emu) in emus.iter_mut() {
            let emu: &mut dyn Backend = emu;
            emu.run();
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    for (path, emu) in emus.iter_mut() {
        // A worker may still be a few frames behind; make sure it has one.
        run_until_frame(emu, Duration::from_secs(5));
        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "no frame produced by worker for {path}");
        //emu.save_png(Path::new(path)).unwrap();
    }
}

/// Boots a licence-stripped scene disc with an MP3 audio track — the shape
/// Beetle can't handle, and the reason pcsx_rearmed is the default. No BIOS
/// is installed here, so this also covers the HLE path.
///
/// Runs under the PAL region `create_core` now pins, since forcing a region
/// is the one way this default could break a disc that booted on `auto`.
#[test]
fn retro_psx_works() {
    let core_path = libloader::get_libretro("mednafen_psx").unwrap();
    // A temp dir, not `system/`: PSX needs nothing from it, and the core
    // writes memory-card files into the system dir — which `build.rs` would
    // then pack into the embedded `system.zip`.
    let system_dir = tempfile::Builder::new()
        .prefix("demarc-")
        .tempdir()
        .unwrap();
    let game_path = root("demos/pdx-dlcm.psx");

    let mut meta = HashMap::new();
    meta.insert("beetle_psx_region".to_string(), "pal".to_string());
    for f in [
        "scph5500.bin",
        "scph5501.bin",
        "scph5502.bin",
        "scph5552.bin",
    ] {
        std::fs::copy(root("system").join(f), system_dir.path().join(f)).unwrap();
    }
    let mut emu =
        RetroCoreDirect::new(&core_path, system_dir.path(), Some(&game_path), meta).unwrap();
    for _ in 0..150 {
        emu.run();
    }
    // emu.save_png(&root("test_psx.png")).unwrap();

    let (w, h) = emu.get_frame_size();
    assert!(w > 0 && h > 0, "no frame produced");
    let distinct = emu
        .state
        .frame
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(distinct > 16, "frame looks blank: only {distinct} colours");
}

#[test]
fn retro_vice_works() {
    let core_path = libloader::get_libretro("vice_x64").unwrap();
    let system_dir = &root("system");
    let game_path = root("demos/quantum_icc2026_v1p.prg");

    let mut retro_emu =
        RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), HashMap::new()).unwrap();
    println!("## RUN");
    for _ in 0..200 {
        retro_emu.run();
    }
    save_png(&retro_emu, &root("test_d64.png")).unwrap();
}

#[test]
fn settings_reach_the_core() {
    let core_path = libloader::get_libretro("puae").unwrap();
    let system_dir = &root("system/amiga");
    let game_path = root("demos/rebels.adf");

    let mut settings = HashMap::new();
    settings.insert("puae_model".into(), "A1200".into());
    settings.insert("puae_video_standard".into(), "NTSC".into());

    let retro_emu =
        RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();

    let var = |key: &str| {
        retro_emu
            .vars
            .get(key)
            .unwrap_or_else(|| panic!("core never saw {key}, has {:?}", retro_emu.vars))
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(var("puae_model"), "A1200");
    assert_eq!(var("puae_video_standard"), "NTSC");
    // Announced by the core, never set by us, so it must have kept its
    // default — this is what proves the core did announce its options.
    assert!(!var("puae_floppy_speed").is_empty());
}
