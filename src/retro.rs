use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::time::Duration;

use anyhow::Result;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    image::Image,
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    render::view::screenshot::{Screenshot, save_to_disk},
};

use ringbuf::{
    HeapProd,
    traits::{Observer, Split, *},
};

use crate::audio::{AudioResampler, init_audio_stream};
use crate::hud::{SpawnToast, ToastType};
use crate::libretro;
use crate::post_process::{BorderMode, PostProcess, ScaleMode};
use crate::retro_emu::{RetroCoreThreaded, RetroEmu};
use crate::utils::{GameInfo, SystemType, WorkingFile, handle_file};
use crate::{AppSettings, Args};

pub struct RetroPlugin {}

#[cfg(target_os = "windows")]
const LIB_EXT: &str = "dll";
#[cfg(target_os = "macos")]
const LIB_EXT: &str = "dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const LIB_EXT: &str = "so";

const CORE_NAME_VICE: &str = "vice_x64sc_libretro";
const CORE_NAME_UAE: &str = "puae_libretro";
const CORE_NAME_AMSTRAD: &str = "cap32_libretro";
const CORE_NAME_ATARI: &str = "hatari_libretro";

/// Audio ring-buffer fill level (in f32 samples) the PI controller aims to
/// hold. Sits between the duplicate (2000) and frame-drop (12000) thresholds,
/// leaving headroom on both sides.
const AUDIO_BUF_MIN: usize = 3000;
const AUDIO_BUF_TARGET: f64 = 8000.0;
const AUDIO_BUF_MAX: usize = 15000;
/// Proportional / integral gains for the audio-buffer controller. Error is
/// normalized by [`AUDIO_BUF_TARGET`], so these are dimensionless.
const AUDIO_PI_KP: f64 = 0.002 * 2.0;
const AUDIO_PI_KI: f64 = 0.0005 * 2.0;
/// Largest fractional sample-rate correction the controller may request
/// (±0.5%), enough to absorb display/audio clock drift without audible pitch.
const AUDIO_RATE_MAX_ADJUST: f64 = 0.005;

/// The `system` directory (BIOS/firmware files) bundled into the binary at
/// build time. Extracted to the user's cache dir on first run.
const SYSTEM_ZIP: &[u8] = include_bytes!("../system.zip");

/// Path to the extracted `system` directory.
///
/// On first call, the embedded [`SYSTEM_ZIP`] is unpacked into
/// `~/.cache/demarc` (creating `~/.cache/demarc/system`) unless it already
/// exists. The result is cached so extraction happens at most once per run.
pub fn system_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let path = ["XDG_CACHE_HOME", "HOME", "HOMEPATH"]
            .iter()
            .find_map(|var| std::env::var_os(var).map(PathBuf::from))
            .unwrap_or("".into());
        let cache = path.join(".cache").join("demarc");
        let system = cache.join("system");
        if !system.exists() {
            std::fs::create_dir_all(&cache).expect("Failed to create demarc cache directory");
            let mut archive = zip::ZipArchive::new(std::io::Cursor::new(SYSTEM_ZIP))
                .expect("Failed to read embedded system.zip");
            archive
                .extract(&cache)
                .expect("Failed to extract system.zip");
        }
        system
    })
    .as_path()
}

#[derive(Resource)]
struct Background {
    handle: Handle<Image>,
    width: u32,
    height: u32,
}

struct Emulator {
    core: Option<RetroCoreThreaded>,
    work_file: WorkingFile,
    games: Vec<PathBuf>,
    current_game: usize,
    run_next: bool,
    next_frame: f64,
    start_time: f64,
    max_time: Option<usize>,
    display_fps: f64,
    match_fps: bool,
    show_info: bool,
    match_frames: usize,
    tags: HashMap<String, String>,
    producer: HeapProd<f32>,
    resampler: AudioResampler,
    _stream: cpal::Stream,
    key_map: HashMap<KeyCode, libretro::retro_key>,
    /// Integral accumulator for the audio-buffer PI controller.
    audio_buf_integral: f64,
    /// Latest fractional sample-rate correction from the PI controller,
    /// applied to the resampler input rate in [`Emulator::update`].
    audio_rate_adjust: f64,
    disk_no: u32,
}

impl Emulator {
    fn build_keycode_map() -> HashMap<KeyCode, libretro::retro_key> {
        use KeyCode::*;
        use libretro::*;

        HashMap::from([
            (Backspace, RETROK_BACKSPACE),
            (Tab, RETROK_TAB),
            (Enter, RETROK_RETURN),
            (Pause, RETROK_PAUSE),
            (Escape, RETROK_ESCAPE),
            (Space, RETROK_SPACE),
            (Quote, RETROK_QUOTE),
            (Comma, RETROK_COMMA),
            (Minus, RETROK_MINUS),
            (Period, RETROK_PERIOD),
            (Slash, RETROK_SLASH),
            (Digit0, RETROK_0),
            (Digit1, RETROK_1),
            (Digit2, RETROK_2),
            (Digit3, RETROK_3),
            (Digit4, RETROK_4),
            (Digit5, RETROK_5),
            (Digit6, RETROK_6),
            (Digit7, RETROK_7),
            (Digit8, RETROK_8),
            (Digit9, RETROK_9),
            (Semicolon, RETROK_SEMICOLON),
            (Equal, RETROK_EQUALS),
            (BracketLeft, RETROK_LEFTBRACKET),
            (Backslash, RETROK_BACKSLASH),
            (BracketRight, RETROK_RIGHTBRACKET),
            (Backquote, RETROK_BACKQUOTE),
            (KeyA, RETROK_a),
            (KeyB, RETROK_b),
            (KeyC, RETROK_c),
            (KeyD, RETROK_d),
            (KeyE, RETROK_e),
            (KeyF, RETROK_f),
            (KeyG, RETROK_g),
            (KeyH, RETROK_h),
            (KeyI, RETROK_i),
            (KeyJ, RETROK_j),
            (KeyK, RETROK_k),
            (KeyL, RETROK_l),
            (KeyM, RETROK_m),
            (KeyN, RETROK_n),
            (KeyO, RETROK_o),
            (KeyP, RETROK_p),
            (KeyQ, RETROK_q),
            (KeyR, RETROK_r),
            (KeyS, RETROK_s),
            (KeyT, RETROK_t),
            (KeyU, RETROK_u),
            (KeyV, RETROK_v),
            (KeyW, RETROK_w),
            (KeyX, RETROK_x),
            (KeyY, RETROK_y),
            (KeyZ, RETROK_z),
            (Delete, RETROK_DELETE),
            (Numpad0, RETROK_KP0),
            (Numpad1, RETROK_KP1),
            (Numpad2, RETROK_KP2),
            (Numpad3, RETROK_KP3),
            (Numpad4, RETROK_KP4),
            (Numpad5, RETROK_KP5),
            (Numpad6, RETROK_KP6),
            (Numpad7, RETROK_KP7),
            (Numpad8, RETROK_KP8),
            (Numpad9, RETROK_KP9),
            (NumpadDecimal, RETROK_KP_PERIOD),
            (NumpadDivide, RETROK_KP_DIVIDE),
            (NumpadMultiply, RETROK_KP_MULTIPLY),
            (NumpadSubtract, RETROK_KP_MINUS),
            (NumpadAdd, RETROK_KP_PLUS),
            (NumpadEnter, RETROK_KP_ENTER),
            (NumpadEqual, RETROK_KP_EQUALS),
            (ArrowUp, RETROK_UP),
            (ArrowDown, RETROK_DOWN),
            (ArrowRight, RETROK_RIGHT),
            (ArrowLeft, RETROK_LEFT),
            (Insert, RETROK_INSERT),
            (Home, RETROK_HOME),
            (End, RETROK_END),
            (PageUp, RETROK_PAGEUP),
            (PageDown, RETROK_PAGEDOWN),
            (F1, RETROK_F1),
            (F2, RETROK_F2),
            (F3, RETROK_F3),
            (F4, RETROK_F4),
            (F5, RETROK_F5),
            (F6, RETROK_F6),
            (F7, RETROK_F7),
            (F8, RETROK_F8),
            (F9, RETROK_F9),
            (F10, RETROK_F10),
            (F11, RETROK_F11),
            (F12, RETROK_F12),
            (F13, RETROK_F13),
            (F14, RETROK_F14),
            (F15, RETROK_F15),
            (NumLock, RETROK_NUMLOCK),
            (CapsLock, RETROK_CAPSLOCK),
            (ScrollLock, RETROK_SCROLLOCK),
            (ShiftRight, RETROK_RSHIFT),
            (ShiftLeft, RETROK_LSHIFT),
            (ControlRight, RETROK_RCTRL),
            (ControlLeft, RETROK_LCTRL),
            (AltRight, RETROK_RALT),
            (AltLeft, RETROK_LALT),
            (SuperLeft, RETROK_LSUPER),
            (SuperRight, RETROK_RSUPER),
            (Help, RETROK_HELP),
            (PrintScreen, RETROK_PRINT),
            (ContextMenu, RETROK_MENU),
            (Power, RETROK_POWER),
            (Undo, RETROK_UNDO),
            (BrowserBack, RETROK_BROWSER_BACK),
            (BrowserForward, RETROK_BROWSER_FORWARD),
            (BrowserRefresh, RETROK_BROWSER_REFRESH),
            (BrowserStop, RETROK_BROWSER_STOP),
            (BrowserSearch, RETROK_BROWSER_SEARCH),
            (BrowserFavorites, RETROK_BROWSER_FAVORITES),
            (BrowserHome, RETROK_BROWSER_HOME),
            (AudioVolumeMute, RETROK_VOLUME_MUTE),
            (AudioVolumeDown, RETROK_VOLUME_DOWN),
            (AudioVolumeUp, RETROK_VOLUME_UP),
            (MediaTrackNext, RETROK_MEDIA_NEXT),
            (MediaTrackPrevious, RETROK_MEDIA_PREV),
            (MediaStop, RETROK_MEDIA_STOP),
            (MediaPlayPause, RETROK_MEDIA_PLAY_PAUSE),
            (LaunchMail, RETROK_LAUNCH_MAIL),
        ])
    }

    pub fn update(&mut self) {
        let Emulator {
            core,
            producer,
            resampler,
            audio_rate_adjust,
            ..
        } = self;
        let Some(core) = core else {
            return;
        };
        // Apply the PI controller's drift correction to the resampler ratio.
        // Done as a relative-ratio nudge (not by changing `from`) so it does
        // not force the resampler to rebuild every frame. A positive adjust
        // makes the resampler emit fewer samples so the ring buffer drains; a
        // negative adjust lets it fill. See `AudioResampler::set_adjust`.
        resampler.set_adjust(*audio_rate_adjust);
        let from = core.sample_rate() as u32;
        core.with_audio(&mut |samples| {
            if samples.is_empty() {
                return;
            }
            if let Err(e) = resampler.process(from, samples, |l, r| {
                producer.push_iter([l, r].into_iter());
            }) {
                warn!("audio resample error: {e}");
            }
        });
    }
}

fn setup_cameras(mut commands: Commands, background: Res<Background>) {
    // Samples the emulator texture directly and renders it to the screen,
    // letting the post-process shader handle scaling to the window.
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        PostProcess {
            source: background.handle.clone(),
            aspect: 0.0, // updated each frame from the core's reported aspect
            aspect_tweak: 1.0,
        },
        RenderLayers::layer(1),
    ));

    // Camera for full res UI on top of screen
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        RenderLayers::layer(2),
    ));
}

fn fix_window(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.mode = WindowMode::Windowed;
}

/// Capture the actual rendered window content and write it to `screenshot.png`.
fn screenshot(commands: &mut Commands, name: impl Into<String>) {
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(name.into()));
}

fn setup_retro(world: &mut World) {
    let (producer, consumer) = ringbuf::HeapRb::<f32>::new(4096 * 8).split();
    let (sample_rate, stream) = init_audio_stream(consumer).unwrap();
    let resampler =
        AudioResampler::new(44100, sample_rate as u32).expect("Failed to create audio resampler");

    let width = 720;
    let height = 574;

    let pixels = vec![0u8; (width * height * 4) as usize];
    let image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::all(),
    );
    let handle = world.resource_mut::<Assets<Image>>().add(image);

    world.insert_resource(Background {
        handle,
        width,
        height,
    });

    let args = world.resource::<Args>();
    let games = &args.files;

    let mut tags = HashMap::new();
    let mut set_var = |name: &str, val: &str| tags.insert(name.into(), val.into());

    if args.aga {
        set_var("puae_model", "A1200");
    }
    if args.ste {
        set_var("hatari_machinetype", "ste");
        set_var("hatari_ramsize", "2");
    }

    if args.high {
        set_var("hatari_ramsize", "8");
        set_var("puae_z3mem_size", "128");
        set_var("puae_fpu_model", "68882");
        set_var("puae_cpu_model", "68030");
        // set_var("puae_cpu_throttle", "10000");
        set_var("puae_cpu_compatibility", "exact");
    }

    for opt in &args.extra_options {
        if let Some((key, val)) = opt.split_once("=") {
            set_var(key.trim(), val.trim());
        }
    }

    world.insert_non_send_resource(Emulator {
        core: None,
        work_file: WorkingFile::default(),
        producer,
        resampler,
        _stream: stream,
        current_game: 0,
        tags,
        match_fps: args.force_vsync,
        show_info: false,
        match_frames: 0,
        display_fps: 0.0,
        next_frame: 0.0,
        start_time: 0.0,
        max_time: args.max_time,
        run_next: !games.is_empty(),
        games: games.clone(),
        key_map: Emulator::build_keycode_map(),
        audio_buf_integral: 0.0,
        audio_rate_adjust: 0.0,
        disk_no: 0,
    });
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
}

fn get_core(sytem_type: SystemType) -> Result<PathBuf, &'static str> {
    let search_path: Vec<PathBuf> = vec![
        "libretro".into(),
        exe_dir().unwrap_or(".".into()),
        "/usr/lib/libretro".into(),
    ];

    let core_name = match sytem_type {
        SystemType::C64 => CORE_NAME_VICE,
        SystemType::Amiga => CORE_NAME_UAE,
        SystemType::Amstrad => CORE_NAME_AMSTRAD,
        SystemType::AtariST => CORE_NAME_ATARI,
        _ => return Err(""),
    };
    let lib_file = format!("{core_name}.{LIB_EXT}");
    for path in search_path.iter() {
        let check = path.join(&lib_file);
        if check.exists() {
            return Ok(check);
        }
    }
    Err(core_name)
}

fn create_core(
    system_type: SystemType,
    game: &Path,
    mut settings: HashMap<String, String>,
) -> Result<RetroCoreThreaded> {
    let mut set_var = |name: &str, val: &str| {
        if !settings.contains_key(name) {
            settings.insert(name.into(), val.into());
        }
    };
    if system_type == SystemType::Amiga {
        set_var("puae_model", "A500");
        //set_var("puae_crop_mode", "4:3");
        set_var("puae_crop", "smaller");
        set_var("puae_horizontal_pos", "-5");
    } else if system_type == SystemType::C64 {
        set_var("vice_jiffydos", "enabled");
        set_var("vice_sid_extra", "none");
        set_var("vice_sid_model", "8580");
        set_var("vice_sound_sample_rate", "44100");
    } else if system_type == SystemType::Amstrad {
        set_var("cap32_statusbar", "disabled");
    } else if system_type == SystemType::AtariST {
        set_var("hatari_forcerefresh", "2");
        set_var("hatari_start_in_mouse_mode", "false");
        set_var("hatari_fastboot", "true");
        //set_var("hatari_video_crop_overscan", "false");
    }
    match get_core(system_type) {
        Ok(core) => RetroCoreThreaded::new(Path::new(&core), system_dir(), Some(game), settings),
        Err(name) => {
            println!("Can not find '{name}'.\nExpected in current directory or /usr/lib/libretro");
            exit(0);
        }
    }
}

fn run_retro(
    mut commands: Commands,
    mut emu: NonSendMut<Emulator>,
    input: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<AppSettings>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mouse_motion: Res<AccumulatedMouseMotion>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut bg: ResMut<Background>,
    time: Res<Time>,
    mut writer: MessageWriter<SpawnToast>,
    mut images: ResMut<Assets<Image>>,
    mut post_process: Query<&mut PostProcess>,
) {
    let Some(image) = images.get_mut(&bg.handle) else {
        return;
    };
    let Some(dst) = image.data.as_mut() else {
        return;
    };

    if emu.run_next && emu.current_game < emu.games.len() {
        emu.core = None;
        //core.unload();
        let game = emu.games[emu.current_game].clone();

        if let Ok(work_file) = handle_file(&game, &emu.tags) {
            if settings.show_info {
                let GameInfo { title, group, year } = &work_file.game_info;
                writer.write(SpawnToast {
                    text: format!("\"{title}\"\n{group}\n{year}"),
                    delay: Duration::from_secs(5),
                    duration: Duration::from_secs(15),
                    toast_type: ToastType::InfoText,
                });
            }
            let core = match create_core(
                work_file.system_type,
                &work_file.path,
                work_file.settings.clone(),
            ) {
                Ok(core) => core,
                Err(e) => {
                    error!("Could not load core for {:?}: {e:#}", work_file.system_type);
                    return;
                }
            };
            emu.core = Some(core);
            emu.work_file = work_file;
            emu.run_next = false;
            emu.current_game += 1;
            emu.next_frame = time.elapsed_secs_f64();
            emu.start_time = time.elapsed_secs_f64();
            trace!("FRAME START");
        }
        return;
    }
    if let Some(mt) = emu.max_time
        && time.elapsed_secs_f64() > emu.start_time + (mt as f64)
    {
        emu.run_next = true;
    };

    let Some(mut core) = emu.core.take() else {
        return;
    };

    let mut mods: u16 = libretro::RETROKMOD_NONE as u16;
    if input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight) {
        mods |= libretro::RETROKMOD_SHIFT as u16;
    }
    if input.pressed(KeyCode::ControlLeft) || input.pressed(KeyCode::ControlRight) {
        mods |= libretro::RETROKMOD_CTRL as u16;
    }
    if input.pressed(KeyCode::AltLeft) || input.pressed(KeyCode::AltRight) {
        mods |= libretro::RETROKMOD_ALT as u16;
    }
    if input.pressed(KeyCode::SuperLeft) || input.pressed(KeyCode::SuperRight) {
        mods |= libretro::RETROKMOD_META as u16;
    }
    if input.pressed(KeyCode::NumLock) {
        mods |= libretro::RETROKMOD_NUMLOCK as u16;
    }
    if input.pressed(KeyCode::CapsLock) {
        mods |= libretro::RETROKMOD_CAPSLOCK as u16;
    }
    if input.pressed(KeyCode::ScrollLock) {
        mods |= libretro::RETROKMOD_SCROLLOCK as u16;
    }

    if input.pressed(KeyCode::AltRight) {
        if input.just_pressed(KeyCode::KeyB) {
            settings.border_mode = if settings.border_mode == BorderMode::Stretch {
                BorderMode::Black
            } else {
                BorderMode::Stretch
            };
        }
        if input.just_pressed(KeyCode::KeyS) {
            settings.scale_mode = match settings.scale_mode {
                ScaleMode::Stretch => ScaleMode::Fit,
                ScaleMode::Fit => ScaleMode::Zoom,
                ScaleMode::Zoom => ScaleMode::Stretch,
            }
        }
        if input.just_pressed(KeyCode::KeyF) {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
        }
        if input.just_pressed(KeyCode::KeyM) {
            core.set_mouse_buttons(true, false, false);
        }
        if input.just_pressed(KeyCode::KeyC) {
            settings.crt_effect = !settings.crt_effect;
        }
        if input.just_pressed(KeyCode::KeyD) {
            emu.disk_no = (emu.disk_no + 1) % core.get_number_of_disks();
            core.set_disk(emu.disk_no);
            let floppy = emu.work_file.system_type == SystemType::C64;
            let d = emu.disk_no + 1;

            writer.write(SpawnToast {
                toast_type: ToastType::BottomLeft,
                duration: Duration::from_millis(1500),
                text: if floppy {
                    format!("\u{f09ef} #{d}")
                } else {
                    format!("\u{f0249} #{d}")
                },
                ..Default::default()
            });
        }
        if input.just_pressed(KeyCode::KeyR) {
            core.reset();
        }
        if input.just_pressed(KeyCode::KeyI) {
            let GameInfo { title, group, year } = &emu.work_file.game_info;
            if emu.show_info {
                writer.write(SpawnToast {
                    text: "".into(),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(5000),
                    toast_type: ToastType::InfoText,
                });
            } else {
                writer.write(SpawnToast {
                    text: format!("\"{title}\"\n{group}\n{year}"),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(5000),
                    toast_type: ToastType::InfoText,
                });
            }
            emu.show_info = !emu.show_info;
        }
        if input.just_pressed(KeyCode::KeyN) {
            emu.run_next = true;
        }
        if input.just_pressed(KeyCode::KeyW) {
            for _ in 0..500 {
                core.run();
                core.with_audio(&mut |_| {});
            }
        }
        if input.just_pressed(KeyCode::KeyP) {
            screenshot(
                &mut commands,
                format!(
                    "{}-{}.png",
                    emu.work_file.game_info.title,
                    time.elapsed_secs() as i32
                ),
            );
        }
    } else {
        for e in input.get_just_pressed() {
            if let Some(code) = emu.key_map.get(e) {
                core.press_key(*code, true, mods);
            }
        }
        for e in input.get_just_released() {
            if let Some(code) = emu.key_map.get(e) {
                core.press_key(*code, false, mods);
            }
        }

        let motion = mouse_motion.delta;
        if motion != Vec2::ZERO {
            core.add_mouse_motion(motion.x, motion.y);
        }
        core.set_mouse_buttons(
            mouse_buttons.pressed(MouseButton::Left),
            mouse_buttons.pressed(MouseButton::Right),
            mouse_buttons.pressed(MouseButton::Middle),
        );
    }
    let delta = time.delta_secs_f64();
    let mut _fps = 60.0;
    if delta > 0.0 {
        _fps = 1.0 / delta;
        if emu.display_fps == 0.0 {
            if _fps > 40.0 || _fps < 500.0 {
                emu.display_fps = _fps;
            }
        } else {
            emu.display_fps = emu.display_fps * 0.95 + _fps * 0.05;
        }
    }

    let ratio = (1.0 - emu.display_fps / core.fps()).abs();
    if ratio < 0.01 && !emu.match_fps {
        emu.match_frames += 1;
        if emu.match_frames >= 8 {
            emu.match_fps = true;
            warn!("Switching to match fps");
        }
    }

    let fps = core.fps();
    let frame_time = if fps > 0.0 {
        1.0 / core.fps()
    } else {
        1.0 / 60.0
    };
    trace!(
        "FRAME FPS {}/{} = {} : t={} AUDIO {}",
        _fps,
        emu.display_fps,
        ratio,
        time.delta_secs(),
        emu.producer.occupied_len()
    );
    if emu.producer.occupied_len() > AUDIO_BUF_MAX {
        warn!("Dropping frame");
        emu.next_frame += frame_time;
        emu.core = Some(core);
        return;
    }

    // PI controller on audio-buffer fill. Output is a fractional
    // sample-rate correction (positive => buffer too full => speed input
    // up so the resampler emits fewer samples and the buffer drains;
    // negative => buffer draining too quickly => slow input down so the
    // resampler emits more samples and the buffer refills). Applied to the
    // resampler input rate in `Emulator::update`.
    let fill = emu.producer.occupied_len() as f64;
    let error = (fill - AUDIO_BUF_TARGET) / AUDIO_BUF_TARGET;
    emu.audio_buf_integral += error * delta;
    // Anti-windup: keep the integral term within the output clamp.
    let i_max = AUDIO_RATE_MAX_ADJUST / AUDIO_PI_KI;
    emu.audio_buf_integral = emu.audio_buf_integral.clamp(-i_max, i_max);
    let adjust = (AUDIO_PI_KP * error + AUDIO_PI_KI * emu.audio_buf_integral)
        .clamp(-AUDIO_RATE_MAX_ADJUST, AUDIO_RATE_MAX_ADJUST);
    emu.audio_rate_adjust = adjust;
    //info!("audio buf fill={fill:.0} err={error:+.3} adjust={adjust:+.5}");

    if emu.match_fps {
        core.run();
    } else {
        let t = time.elapsed_secs_f64();
        while t >= emu.next_frame {
            core.run();
            emu.next_frame += frame_time;
        }
    }

    // For safety
    if emu.producer.occupied_len() < AUDIO_BUF_MIN {
        core.run();
        warn!("Duplicating frame");
        //emu.core = Some(core);
        //return;
    }

    let bg_w = bg.width as usize;
    let bg_h = bg.height as usize;

    core.with_frame(&mut |w, h, frame| {
        let copy_w = w.min(bg_w);
        let copy_h = h.min(bg_h);
        for y in 0..copy_h {
            let src_off = y * w * 4;
            let dst_off = y * bg_w * 4;
            dst[dst_off..dst_off + copy_w * 4]
                .copy_from_slice(&frame[src_off..src_off + copy_w * 4]);
        }
    });
    // For some reason we need to compensate the hatari aspect
    let aspect = if emu.work_file.system_type == SystemType::AtariST {
        let (w, h) = core.get_frame_size();
        if h > 0 {
            w as f32 / h as f32
        } else {
            core.aspect_ratio()
        }
    } else {
        core.aspect_ratio()
    };
    for mut pp in &mut post_process {
        pp.aspect = aspect;
    }

    let (w, h) = core.get_frame_size();

    emu.core = Some(core);
    emu.update();

    if w != bg_w || h != bg_h {
        debug!("SIZE CHANGE TO {w} {h}");
        bg.width = w as u32;
        bg.height = h as u32;
        if let Some(image) = images.get_mut(&bg.handle) {
            // Recreate with new dimensions
            info!("RECREATE");
            *image = Image::new(
                Extent3d {
                    width: w as u32,
                    height: h as u32,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                vec![0u8; w * h * 4], // RGBA zeros
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
        }
    }
}

impl Plugin for RetroPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_retro, setup_cameras, fix_window).chain());
        app.add_systems(Update, run_retro);
    }
}
