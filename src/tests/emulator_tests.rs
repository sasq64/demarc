use std::sync::Mutex;
use std::time::Duration;

use ringbuf::traits::{Producer, Split};

use super::*;
use crate::emu_file::CompactDate;
use crate::fake_backend::FakeBackend;

/// An emulator running `core`, with the key map `Emulator::new` gives it.
fn emulator(core: &FakeBackend) -> Emulator {
    Emulator {
        core: core.boxed(),
        key_map: Emulator::build_keycode_map(),
        ..Default::default()
    }
}

/// A clock `ms` milliseconds past the previous frame.
fn time_after(ms: u64) -> Time {
    let mut time = Time::default();
    time.advance_by(Duration::from_millis(ms));
    time
}

fn rgba(r: u8, g: u8, b: u8) -> u32 {
    u32::from_ne_bytes([r, g, b, 255])
}

#[test]
fn input_mode_cycles_through_both_joystick_ports() {
    let mode = InputMode::default();
    assert_eq!(mode.joypad_port(), None);
    let mode = mode.next();
    assert_eq!((mode, mode.joypad_port()), (InputMode::Joystick1, Some(0)));
    let mode = mode.next();
    assert_eq!((mode, mode.joypad_port()), (InputMode::Joystick2, Some(1)));
    assert_eq!(mode.next(), InputMode::Keyboard);
}

#[test]
fn new_emulator_owns_a_raw_rgba_texture_and_wants_a_file() {
    let mut images = Assets::<Image>::default();
    let emu = Emulator::new(&mut images, Some(30), true, false);

    let image = images.get(&emu.image).expect("the texture is registered");
    assert_eq!(image.size(), UVec2::new(emu.width, emu.height));
    assert_eq!(image.texture_descriptor.format, TextureFormat::Rgba8Unorm);
    assert_eq!(
        image.data.as_ref().map(Vec::len),
        Some((emu.width * emu.height * 4) as usize)
    );
    assert!(emu.run_next, "a fresh emulator loads the first file");
    assert_eq!(emu.max_time, Some(30));
    assert!(emu.color_cycle);
    assert!(emu.core.is_none());
}

#[test]
fn keys_reach_the_core_with_the_held_modifiers() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let mut input = ButtonInput::<KeyCode>::default();
    input.press(KeyCode::ShiftLeft);
    input.press(KeyCode::KeyA);

    emu.feed_inputs(&input, &default(), &default(), None);

    let shift = libretro::RETROKMOD_SHIFT as u16;
    assert!(
        core.lock()
            .keys
            .contains(&(libretro::RETROK_a, true, shift))
    );
    assert!(core.lock().joypad.is_empty());

    input.clear();
    input.release(KeyCode::KeyA);
    core.lock().keys.clear();
    emu.feed_inputs(&input, &default(), &default(), None);
    assert_eq!(core.lock().keys, [(libretro::RETROK_a, false, shift)]);
}

#[test]
fn every_modifier_is_reported() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let mut input = ButtonInput::<KeyCode>::default();
    for key in [
        KeyCode::ControlLeft,
        KeyCode::AltLeft,
        KeyCode::SuperRight,
        KeyCode::NumLock,
        KeyCode::CapsLock,
        KeyCode::ScrollLock,
    ] {
        input.press(key);
    }
    input.clear();
    input.press(KeyCode::Space);

    emu.feed_inputs(&input, &default(), &default(), None);

    let mods = libretro::RETROKMOD_CTRL
        | libretro::RETROKMOD_ALT
        | libretro::RETROKMOD_META
        | libretro::RETROKMOD_NUMLOCK
        | libretro::RETROKMOD_CAPSLOCK
        | libretro::RETROKMOD_SCROLLOCK;
    assert_eq!(
        core.lock().keys,
        [(libretro::RETROK_SPACE, true, mods as u16)]
    );
}

/// F12, RightCtrl and RightAlt are the frontend's own keys.
#[test]
fn frontend_hotkeys_never_reach_the_core() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let mut input = ButtonInput::<KeyCode>::default();
    for key in [KeyCode::F12, KeyCode::ControlRight, KeyCode::AltRight] {
        input.press(key);
    }
    emu.feed_inputs(&input, &default(), &default(), None);

    input.clear();
    input.release_all();
    emu.feed_inputs(&input, &default(), &default(), None);

    assert!(core.lock().keys.is_empty());
}

#[test]
fn joystick_mode_turns_cursor_keys_into_a_joypad() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.input_mode = InputMode::Joystick2;
    let mut input = ButtonInput::<KeyCode>::default();
    input.press(KeyCode::ArrowUp);
    input.press(KeyCode::KeyQ);

    emu.feed_inputs(&input, &default(), &default(), None);

    assert_eq!(
        core.lock().joypad,
        [(1, libretro::RETRO_DEVICE_ID_JOYPAD_UP, true)]
    );
    // Anything that is not a joypad button is still typed.
    assert_eq!(core.lock().keys, [(libretro::RETROK_q, true, 0)]);

    input.clear();
    input.release(KeyCode::ArrowUp);
    emu.feed_inputs(&input, &default(), &default(), None);
    assert_eq!(
        core.lock().joypad.last(),
        Some(&(1, libretro::RETRO_DEVICE_ID_JOYPAD_UP, false))
    );
}

#[test]
fn joypad_buttons_cover_the_fire_keys() {
    use libretro::*;
    for (key, id) in [
        (KeyCode::ArrowDown, RETRO_DEVICE_ID_JOYPAD_DOWN),
        (KeyCode::ArrowLeft, RETRO_DEVICE_ID_JOYPAD_LEFT),
        (KeyCode::ArrowRight, RETRO_DEVICE_ID_JOYPAD_RIGHT),
        (KeyCode::KeyO, RETRO_DEVICE_ID_JOYPAD_A),
        (KeyCode::KeyX, RETRO_DEVICE_ID_JOYPAD_B),
        (KeyCode::Enter, RETRO_DEVICE_ID_JOYPAD_START),
        (KeyCode::Backspace, RETRO_DEVICE_ID_JOYPAD_SELECT),
    ] {
        assert_eq!(Emulator::joypad_button(key), Some(id), "{key:?}");
    }
    assert_eq!(Emulator::joypad_button(KeyCode::Space), None);
}

#[test]
fn mouse_motion_position_and_buttons_are_forwarded() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let keys = ButtonInput::<KeyCode>::default();
    let mut buttons = ButtonInput::<MouseButton>::default();
    buttons.press(MouseButton::Right);
    let motion = AccumulatedMouseMotion {
        delta: Vec2::new(3.0, -2.0),
    };

    emu.feed_inputs(&keys, &buttons, &motion, Some(Vec2::new(0.25, 0.75)));

    let state = core.lock();
    assert_eq!(state.motion, [(3.0, -2.0)]);
    assert_eq!(state.pointer, [(0.25, 0.75)]);
    assert_eq!(state.buttons, [(false, true, false)]);
}

#[test]
fn a_still_mouse_sends_no_motion() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);

    emu.feed_inputs(&default(), &default(), &default(), None);

    let state = core.lock();
    assert!(state.motion.is_empty());
    assert!(state.pointer.is_empty());
    assert_eq!(state.buttons, [(false, false, false)]);
}

/// A click injected with `set_mouse_buttons` is held for one frame only.
#[test]
fn an_injected_click_lasts_one_frame() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.set_mouse_buttons(1);

    emu.feed_inputs(&default(), &default(), &default(), None);
    emu.feed_inputs(&default(), &default(), &default(), None);

    assert_eq!(
        core.lock().buttons,
        [(true, false, false), (false, false, false)]
    );
}

#[test]
fn disk_and_reset_requests_go_to_the_core() {
    let core = FakeBackend::default();
    core.lock().disks = 3;
    let mut emu = emulator(&core);

    assert_eq!(emu.get_number_of_disks(), 3);
    emu.set_disk(2);
    emu.reset();

    assert_eq!(core.lock().disk, 2);
    assert_eq!(core.lock().resets, 1);
}

#[test]
fn focus_is_passed_on_when_there_is_a_core() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.focus(ViewFocus::Invisible);
    assert_eq!(core.lock().focus, [ViewFocus::Invisible]);

    Emulator::default().focus(ViewFocus::Focus);
}

#[test]
fn info_names_the_release_its_year_and_its_system() {
    let mut emu = Emulator::default();
    emu.work_file.set_meta("system", "C64");
    emu.emu_file.game_info = GameInfo {
        title: "Deus Ex Machina",
        group: "Crest",
        date: CompactDate::new(2000, 10, 1),
        category: "demo",
        ..Default::default()
    };
    assert_eq!(
        emu.get_info(),
        "\"Deus Ex Machina\"\nCrest (2000)\nC64 demo"
    );

    emu.favorite = true;
    assert!(
        emu.get_info()
            .starts_with("\"Deus Ex Machina\" \u{f02d1}\n")
    );
}

#[test]
fn info_leaves_out_what_is_not_known() {
    let mut emu = Emulator::default();
    emu.emu_file.game_info.title = "Untitled";
    assert_eq!(emu.get_info(), "\"Untitled\"\n\n???");
}

#[test]
fn a_core_that_describes_itself_replaces_the_system_line() {
    let core = FakeBackend::default();
    core.lock().info = Some("4 channel MOD".into());
    let mut emu = emulator(&core);
    emu.work_file.set_meta("system", "Amiga");
    emu.emu_file.game_info.category = "music";

    assert!(emu.get_info().ends_with("\n4 channel MOD"));
}

#[test]
fn save_png_writes_the_cores_current_frame() {
    let core = FakeBackend::default();
    core.lock().frame = VideoFrame {
        width: 2,
        height: 1,
        pixels: Arc::new(vec![rgba(255, 0, 0), rgba(0, 0, 255)]),
    };
    let emu = emulator(&core);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");

    emu.save_png(&path).unwrap();

    let png = image::open(&path).unwrap().to_rgba8();
    assert_eq!(png.dimensions(), (2, 1));
    assert_eq!(png.get_pixel(0, 0).0, [255, 0, 0, 255]);
    assert_eq!(png.get_pixel(1, 0).0, [0, 0, 255, 255]);
}

#[test]
fn save_png_without_a_core_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shot.png");

    Emulator::default().save_png(&path).unwrap();

    assert!(!path.exists());
}

#[test]
fn update_reports_whether_audio_arrived() {
    assert!(!Emulator::default().update());

    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    assert!(!emu.update());
    assert!(!emu.audio_seen, "an empty batch is not audio");

    core.lock().audio = vec![1, 2, 3, 4];
    assert!(emu.update());
    assert!(emu.audio_seen);

    // Latched: a silent frame later does not make it a core without audio.
    core.lock().audio.clear();
    assert!(!emu.update());
    assert!(emu.audio_seen);
}

#[test]
fn skip_starts_the_core_skipping() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);

    emu.skip(500);

    assert_eq!(core.lock().skipped, 500);
    assert!(emu.skipping);
    assert!(!emu.paused);
}

/// A still image has nothing to skip through, so it stops on the result.
#[test]
fn skipping_an_image_pauses_it() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.is_image = true;

    emu.skip(1);

    assert!(emu.paused);
}

#[test]
fn skip_finished_fires_once_when_the_core_stops_skipping() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    assert!(!emu.skip_finished(), "nothing was skipped");

    emu.skip(500);
    core.lock().state = STATE_SKIPPING;
    assert!(!emu.skip_finished());

    core.lock().state = 0;
    assert!(emu.skip_finished());
    assert!(!emu.skip_finished());
}

#[test]
fn a_skip_without_a_core_still_takes_the_indicator_down() {
    let mut emu = Emulator::default();

    emu.skip(500);

    assert!(!emu.skipping);
    assert!(emu.skip_finished());
}

#[test]
fn reset_idle_restarts_the_idle_timer() {
    let mut emu = Emulator {
        idle_time: 30.0,
        ..Default::default()
    };

    emu.reset_idle(&time_after(5000));

    assert_eq!(emu.idle_time, 0.0);
    assert_eq!(emu.last_active_time, 5.0);
}

#[test]
fn display_fps_is_measured_then_smoothed() {
    let mut emu = Emulator::default();

    // Too slow to be a display refresh: a hitch, not a measurement.
    assert!(emu.run(&time_after(1000)));
    assert_eq!(emu.display_fps, 0.0);

    emu.run(&time_after(20));
    assert!((emu.display_fps - 50.0).abs() < 1e-6);

    emu.run(&time_after(10));
    assert!((emu.display_fps - 52.5).abs() < 1e-6);
}

#[test]
fn idle_time_counts_while_the_core_is_idle() {
    let core = FakeBackend::default();
    core.lock().idle = true;
    let mut emu = emulator(&core);
    let mut time = Time::default();

    time.advance_by(Duration::from_secs(3));
    emu.run(&time);
    assert_eq!(emu.idle_time, 3.0);

    core.lock().idle = false;
    time.advance_by(Duration::from_secs(1));
    emu.run(&time);
    assert_eq!(emu.idle_time, 0.0);
}

#[test]
fn a_paused_core_stands_still_without_going_idle() {
    let core = FakeBackend::default();
    core.lock().idle = true;
    let mut emu = emulator(&core);
    emu.paused = true;

    assert!(emu.run(&time_after(3000)));

    assert_eq!(core.lock().runs, 0);
    assert_eq!(emu.idle_time, 0.0);
    // Paused time counts towards neither the idle timeout nor `max_time`.
    assert_eq!(emu.start_time, 3.0);
    assert_eq!(emu.next_frame, 3.0);
}

/// Still images are paused by default and rely on both timers to advance.
#[test]
fn a_paused_image_still_times_out() {
    let core = FakeBackend::default();
    core.lock().idle = true;
    let mut emu = emulator(&core);
    emu.paused = true;
    emu.is_image = true;

    emu.run(&time_after(3000));

    assert_eq!(emu.idle_time, 3.0);
    assert_eq!(emu.start_time, 0.0);
}

#[test]
fn speed_test_steps_once_per_update_whatever_the_clock_says() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.speed_test = true;
    emu.paused = true;

    emu.run(&Time::default());
    emu.run(&Time::default());
    emu.run(&time_after(1000));

    assert_eq!(core.lock().runs, 3);
}

/// 50 fps core, 90 ms behind: the frames due at 0, 20, 40, 60 and 80 ms.
#[test]
fn the_core_catches_up_with_the_clock() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let mut time = Time::default();

    time.advance_by(Duration::from_millis(90));
    emu.run(&time);
    assert_eq!(core.lock().runs, 5);

    time.advance_by(Duration::from_millis(5));
    emu.run(&time);
    assert_eq!(core.lock().runs, 5, "the next frame is not due yet");
}

#[test]
fn a_core_reporting_no_fps_is_paced_at_60() {
    let core = FakeBackend::default();
    core.lock().fps = 0.0;
    let mut emu = emulator(&core);

    emu.run(&time_after(40));

    assert_eq!(core.lock().runs, 3);
}

#[test]
fn a_display_at_the_cores_rate_is_followed_frame_for_frame() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let mut time = Time::default();

    for _ in 0..7 {
        time.advance_by(Duration::from_millis(20));
        emu.run(&time);
    }
    assert!(!emu.match_fps);
    time.advance_by(Duration::from_millis(20));
    emu.run(&time);
    assert!(emu.match_fps);

    // One step per update from here on, however late the update is.
    let runs = core.lock().runs;
    time.advance_by(Duration::from_millis(200));
    emu.run(&time);
    assert_eq!(core.lock().runs, runs + 1);
}

/// With no stream the sink reports a level under `AUDIO_BUF_MIN`.
#[test]
fn a_dry_audio_buffer_steps_an_extra_frame() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.audio_seen = true;

    emu.run(&Time::default());

    assert_eq!(core.lock().runs, 2);
}

#[test]
fn a_silent_core_is_not_stepped_twice() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);

    emu.run(&Time::default());

    assert_eq!(core.lock().runs, 1);
}

#[test]
fn a_full_audio_buffer_drops_the_frame() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    let (mut producer, _consumer) = ringbuf::HeapRb::<f32>::new(AUDIO_BUF_MAX * 2).split();
    producer.push_iter(std::iter::repeat_n(0.0, AUDIO_BUF_MAX + 1));
    emu.sink.producer = Some(Mutex::new(producer));

    assert!(emu.run(&Time::default()));

    assert_eq!(core.lock().runs, 0);
    assert_eq!(emu.next_frame, 1.0 / 50.0);
}

#[test]
fn a_skip_ends_when_audio_comes_back() {
    let core = FakeBackend::default();
    let mut emu = emulator(&core);
    emu.audio_seen = true;
    emu.skip(500);
    emu.audio_rate_adjust = -0.01;

    emu.run(&Time::default());
    assert!(emu.skipping, "no audio yet, so the skip is still running");
    assert_eq!(core.lock().runs, 1, "no catch-up frame during a skip");

    core.lock().audio = vec![0; 64];
    emu.run(&Time::default());
    assert!(!emu.skipping);
    assert_eq!(emu.audio_rate_adjust, 0.0);
}

#[test]
fn the_key_map_covers_letters_digits_and_cursor_keys() {
    let map = Emulator::build_keycode_map();
    assert_eq!(map[&KeyCode::KeyZ], libretro::RETROK_z);
    assert_eq!(map[&KeyCode::Digit7], libretro::RETROK_7);
    assert_eq!(map[&KeyCode::ArrowLeft], libretro::RETROK_LEFT);
    assert_eq!(map[&KeyCode::NumpadEnter], libretro::RETROK_KP_ENTER);
    assert_eq!(map[&KeyCode::F12], libretro::RETROK_F12);
}
