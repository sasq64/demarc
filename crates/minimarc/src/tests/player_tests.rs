use super::*;
use database::emu_file::EmuFile;
use std::path::Path;

fn root(rel: &str) -> PathBuf {
    Path::new(env!("DEMARC_ROOT")).join(rel)
}

/// Step the app's loader until `done`, as the event loop would.
fn wait_for(app: &mut App, what: &str, done: impl Fn(&App) -> bool) {
    let start = Instant::now();
    while !done(app) {
        app.poll_loader();
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "{what} never happened"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn picking_an_entry_starts_it_and_a_second_pick_replaces_it() {
    retro_core::set_system_dir(root("system"));
    let mut app = App::new(root("testdata/demos.txt"));
    wait_for(&mut app, "the database", |app| app.ui.is_open());

    for (demo, frames) in [("quantum.prg", 60), ("Skaaneland.zip", 80)] {
        app.loader.load_demo(EmuFile {
            path: database::emu_file::FileSource::Path(root("testdata/c64").join(demo)),
            ..Default::default()
        });
        app.core_frames = u32::MAX;
        wait_for(&mut app, demo, |app| app.core_frames == 0);
        let core = app.core.as_mut().unwrap();
        for _ in 0..frames {
            core.run();
        }
        assert!(core.get_frame_size().0 > 0, "{demo} drew nothing");
    }
    // The disk set came with the fast-load cartridge, whose menu takes keys.
    assert!(!app.startup_keys.is_empty());
}

#[test]
fn the_picker_draws_and_enters_a_list() {
    let mut ui = Ui::new();
    let files: &'static [EmuFile] = Box::leak(Box::new([EmuFile::default()]));
    ui.set_db("CSDb".into(), files).unwrap();
    let ctx = ui.ctx.clone();
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |egui_ui| {
            assert!(ui.draw(egui_ui.ctx()).is_none());
        });
        output.textures_delta.clear();
    }
    assert!(ui.is_open());
}
