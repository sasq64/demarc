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
    let mut app = App::new(vec![root("testdata/demos.txt")]);
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
    ui.set_db(vec![("CSDb".into(), files)]).unwrap();
    let ctx = ui.ctx.clone();
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |egui_ui| {
            assert!(ui.draw(egui_ui.ctx()).is_none());
        });
        output.textures_delta.clear();
    }
    assert!(ui.is_open());
}

#[test]
fn the_key_bar_finds_the_button_under_a_finger() {
    let mut ui = Ui::new();
    ui.keys_open = true;
    let ctx = ui.ctx.clone();
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |egui_ui| {
            ui.draw(egui_ui.ctx());
        });
        output.textures_delta.clear();
    }
    let screen = ctx.content_rect();
    let scale = ctx.pixels_per_point() as f64;
    let at = |x: f32, y: f32| ui.key_at(x as f64 * scale, y as f64 * scale);
    let (w, bottom) = (screen.width() / 8.0, screen.bottom());
    assert_eq!(at(w * 0.5, bottom - 90.0), Some(Button::F1));
    assert_eq!(at(w * 7.5, bottom - 90.0), Some(Button::Csdb));
    assert_eq!(at(w * 0.5, bottom - 30.0), Some(Button::Disk));
    assert_eq!(at(w * 7.5, bottom - 30.0), Some(Button::Back));
    assert_eq!(at(w * 0.5, screen.top() + 10.0), None);
}

#[test]
fn an_amiga_key_bar_has_the_mouse_buttons() {
    let mut ui = Ui::new();
    ui.keys_open = true;
    ui.amiga = true;
    let ctx = ui.ctx.clone();
    for _ in 0..3 {
        let mut output = ctx.run_ui(egui::RawInput::default(), |egui_ui| {
            ui.draw(egui_ui.ctx());
        });
        output.textures_delta.clear();
    }
    let screen = ctx.content_rect();
    let scale = ctx.pixels_per_point() as f64;
    let at = |x: f32, y: f32| ui.key_at(x as f64 * scale, y as f64 * scale);
    let (w, bottom) = (screen.width() / 8.0, screen.bottom());
    assert_eq!(at(w * 0.5, bottom - 90.0), Some(Button::MouseLeft));
    assert_eq!(at(w * 2.5, bottom - 90.0), Some(Button::Pointer));
    assert_eq!(at(w * 3.5, bottom - 90.0), Some(Button::Esc));
}

#[test]
fn the_crt_shader_validates() {
    use wgpu::naga;
    let module = naga::front::wgsl::parse_str(CRT_WGSL).unwrap();
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .unwrap();
}

#[test]
fn override_key_names_resolve_to_retro_keys() {
    assert_eq!(key_by_name("Enter"), Some(libretro::RETROK_RETURN));
    assert_eq!(key_by_name("NumpadEnter"), Some(libretro::RETROK_RETURN));
    assert_eq!(key_by_name("KeyA"), Some(libretro::RETROK_a));
    assert_eq!(key_by_name("1"), Some(libretro::RETROK_1));
    assert_eq!(key_by_name("b"), Some(libretro::RETROK_b));
    assert_eq!(key_by_name("NoSuchKey"), None);
    assert_eq!(retro_key(KeyCode::F7), Some(libretro::RETROK_F7));
}
