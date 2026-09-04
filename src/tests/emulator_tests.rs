use std::time::{Duration, Instant};

use bevy::MinimalPlugins;
use clap::Parser;

use super::*;
use crate::{
    Args,
    emu_file::{FileSource, UrlList},
};

/// Spins up the task pools `load_async` needs, and nothing else.
fn task_pools() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.update();
    app
}

/// The system table with stock settings, which is all `update_load` needs
/// to hand the resolved file on to `load`.
fn systems() -> NewSys {
    NewSys::new(&Args::parse_from(["demarc"]))
}

/// Pumps `update_load` until it stops reporting `Pending`.
fn drive_load(emu: &mut Emulator, sys: &NewSys) -> LoadStatus {
    let time = Time::default();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match emu.update_load(&time, sys) {
            LoadStatus::Pending => {
                assert!(emu.is_loading(), "a pending load must report as loading");
                assert!(Instant::now() < deadline, "load never finished");
                std::thread::yield_now();
            }
            status => return status,
        }
    }
}

#[test]
fn nothing_pending_is_idle() {
    let _app = task_pools();
    let mut emu = Emulator::default();
    assert!(!emu.is_loading());
    assert!(matches!(
        emu.update_load(&Time::default(), &systems()),
        LoadStatus::Idle
    ));
}

/// A failed download surfaces as `Done`, carrying the entry's title so the
/// frontend can name it — `work_file` still describes the previous load.
///
/// Port 1 refuses immediately, so this fails fast without leaving the host.
#[test]
fn a_failed_download_finishes_the_load() {
    let _app = task_pools();
    let mut emu = Emulator::default();

    emu.load_async(
        &EmuFile {
            path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
            game_info: GameInfo {
                title: "Unreachable",
                ..Default::default()
            },
            ..Default::default()
        },
        None,
    );
    assert!(emu.is_loading(), "the download starts in flight");

    let LoadStatus::Done { title, result } = drive_load(&mut emu, &systems()) else {
        panic!("expected the load to finish");
    };
    assert_eq!(title, "Unreachable");
    assert!(result.is_err(), "a refused connection cannot load");
    assert!(!emu.is_loading(), "the pending load is cleared either way");
    // Nothing was swapped in, so the emulator still has no core.
    assert!(emu.core.is_none());
}

/// The whole point of `update_load`: what reaches the main thread is an
/// unpacked `WorkFile`, never a URL, so it never blocks on the network.
#[test]
fn a_local_path_reaches_load_unchanged() {
    let _app = task_pools();
    let dir = tempfile::tempdir().unwrap();
    // Nothing any system claims, so `load` fails once it gets there — after
    // the resolution step this test is about.
    let game = dir.path().join("demo.xyz");
    std::fs::write(&game, b"not really anything").unwrap();

    let mut emu = Emulator::default();
    emu.load_async(
        &EmuFile {
            path: FileSource::Path(game),
            ..Default::default()
        },
        None,
    );

    assert!(matches!(
        drive_load(&mut emu, &systems()),
        LoadStatus::Done { .. }
    ));
    assert!(!emu.is_loading());
}

/// A packed release comes through the async path unpacked — the job now
/// does that (which is what keeps a cross-fade from stuttering on it), and
/// what reaches the main thread is the directory it was unpacked into.
///
/// Observed through the failure message, which lists the archive's contents
/// rather than naming the zip.
#[test]
fn an_archive_is_unpacked_before_the_main_thread_sees_it() {
    use std::io::Write;

    let _app = task_pools();
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("demo.zip");
    let mut zw = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    // Nothing any system claims, so the load fails once it gets there —
    // after the unpacking step this test is about.
    zw.start_file("inside.xyz", opts).unwrap();
    zw.write_all(b"not really anything").unwrap();
    zw.finish().unwrap();

    let mut emu = Emulator::default();
    emu.load_async(
        &EmuFile {
            path: FileSource::Path(archive),
            ..Default::default()
        },
        None,
    );

    let LoadStatus::Done { result, .. } = drive_load(&mut emu, &systems()) else {
        panic!("expected the load to finish");
    };
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("inside.xyz") && !err.contains("demo.zip"),
        "the archive should have been unpacked in the job, got: {err}"
    );
}

/// `load_async` consumes the advance request, so the frontend asks for the
/// load once rather than on every frame of the download; a failure hands it
/// back, which is how tv mode steps past a dead link.
#[test]
fn the_advance_request_is_taken_and_returned_on_failure() {
    let _app = task_pools();
    let mut emu = Emulator {
        run_next: true,
        ..Default::default()
    };

    emu.load_async(
        &EmuFile {
            path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
            ..Default::default()
        },
        None,
    );
    assert!(
        !emu.run_next && !emu.run_prev,
        "the request is consumed while the download runs"
    );

    assert!(matches!(
        drive_load(&mut emu, &systems()),
        LoadStatus::Done { .. }
    ));
    assert!(emu.run_next, "a failed load re-arms the advance");
}

/// The backwards direction survives a failure too, so an explicit PrevFile
/// onto a dead link keeps going backwards rather than reversing.
#[test]
fn a_failed_load_re_arms_the_direction_it_had() {
    let _app = task_pools();
    let mut emu = Emulator {
        run_prev: true,
        ..Default::default()
    };

    emu.load_async(
        &EmuFile {
            path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
            ..Default::default()
        },
        None,
    );
    assert!(matches!(
        drive_load(&mut emu, &systems()),
        LoadStatus::Done { .. }
    ));
    assert!(emu.run_prev && !emu.run_next);
}

/// Starting a second load replaces the first: only one download can be
/// outstanding, so the frontend can't stack them up frame after frame.
#[test]
fn a_second_load_replaces_the_first() {
    let _app = task_pools();
    let mut emu = Emulator::default();
    let entry = |title: &'static str| EmuFile {
        path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
        game_info: GameInfo {
            title,
            ..Default::default()
        },
        ..Default::default()
    };

    emu.load_async(&entry("First"), None);
    emu.load_async(&entry("Second"), None);

    let sys = systems();
    let LoadStatus::Done { title, .. } = drive_load(&mut emu, &sys) else {
        panic!("expected the load to finish");
    };
    assert_eq!(title, "Second", "the newer load is the one that lands");
    assert!(matches!(
        emu.update_load(&Time::default(), &sys),
        LoadStatus::Idle
    ));
}
