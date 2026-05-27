use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
};

use ringbuf::{
    HeapProd,
    traits::{Observer, Split, *},
};

use crate::audio::{AudioResampler, init_audio_stream};
use crate::hud::SpawnToast;
use crate::libretro;
use crate::post_process::{BorderMode, PostProcess, ScaleMode};
use crate::retro_emu::RetroCore;
use crate::utils::{SystemType, WorkingFile, get_sytem_type, handle_file};
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

/// The `system` directory (BIOS/firmware files) bundled into the binary at
/// build time. Extracted to the user's cache dir on first run.
const SYSTEM_ZIP: &[u8] = include_bytes!("../system.zip");

/// Path to the extracted `system` directory.
///
/// On first call, the embedded [`SYSTEM_ZIP`] is unpacked into
/// `~/.cache/demarc` (creating `~/.cache/demarc/system`) unless it already
/// exists. The result is cached so extraction happens at most once per run.
fn system_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var_os("HOME").expect("HOME is not set")).join(".cache")
            })
            .join("demarc");
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
    core: RetroCore,
    work_file: WorkingFile,
    games: Vec<PathBuf>,
    current_game: usize,
    run_next: bool,
    next_frame: f64,
    display_fps: f64,
    match_fps: bool,
    tags: HashMap<String, String>,
    producer: HeapProd<f32>,
    resampler: AudioResampler,
    _stream: cpal::Stream,
    key_map: HashMap<KeyCode, libretro::retro_key>,
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
            ..
        } = self;
        let from = (core.sample_rate() * 1.0) as u32;
        core.with_audio(|samples| {
            if let Err(e) = resampler.process(from, samples, |l, r| {
                producer.push_iter([l, r].into_iter());
            }) {
                warn!("audio resample error: {e}");
            }
        });
    }
}

struct M3u {
    tags: HashMap<String, String>,
    files: Vec<PathBuf>,
}

fn parse_m3u(path: &Path) -> Result<M3u> {
    let contents = std::fs::read_to_string(path)?;
    let mut tags = HashMap::new();
    let mut files: Vec<PathBuf> = vec![];
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let mut remaining = rest;
            while let Some(eq) = remaining.find("=\"") {
                let key_start = remaining[..eq]
                    .rfind(|c: char| c.is_whitespace() || c == ',')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let key = remaining[key_start..eq].trim();
                let after_quote = &remaining[eq + 2..];
                let Some(end) = after_quote.find('"') else {
                    break;
                };
                let value = &after_quote[..end];
                if !key.is_empty() {
                    tags.insert(key.to_string(), value.to_string());
                }
                remaining = &after_quote[end + 1..];
            }
        } else if !line.starts_with('#') {
            files.push(line.into());
        }
    }
    Ok(M3u { tags, files })
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

    let settings: HashMap<String, String> =
        [("vice_cartridge".into(), "rr38ppal.crt".into())].into();

    let core_path = get_core(SystemType::C64).unwrap();
    let args = world.resource::<Args>();
    let games = &args.games;
    let core = RetroCore::new(Path::new(&core_path), system_dir(), None, settings)
        .expect("Failed to load libretro core");

    let mut tags = HashMap::new();
    let mut set_var = |name: &str, val: &str| tags.insert(name.into(), val.into());
    if args.aga {
        set_var("puae_model", "A1200");
    }

    if args.high {
        set_var("puae_z3mem_size", "128");
        set_var("puae_fpu_model", "68881");
        set_var("puae_cpu_model", "68030");
        // set_var("puae_cpu_throttle", "10000");
        //set_var("puae_cpu_compatibility", "exact");
    }

    world.insert_non_send_resource(Emulator {
        core,
        work_file: WorkingFile::default(),
        producer,
        resampler,
        _stream: stream,
        current_game: 0,
        tags,
        match_fps: false,
        display_fps: 0.0,
        next_frame: 0.0,
        run_next: !games.is_empty(),
        games: games.clone(),
        key_map: Emulator::build_keycode_map(),
    });
}

fn get_core(sytem_type: SystemType) -> Option<PathBuf> {
    let search_path: Vec<PathBuf> = vec![".".into(), "/usr/lib/libretro".into()];
    let core_name = match sytem_type {
        SystemType::C64 => CORE_NAME_VICE,
        SystemType::Amiga => CORE_NAME_UAE,
        _ => CORE_NAME_UAE,
    };
    let lib_file = format!("{core_name}.{LIB_EXT}");
    for path in search_path.iter() {
        let check = path.join(&lib_file);
        if check.exists() {
            return Some(check);
        }
    }
    None
}
/// Find a direct child of `dir` whose name matches `name` case-insensitively.
/// Amiga volumes are case-insensitive, so a host directory meant to act as one
/// may use any casing (e.g. `S/Startup-Sequence`).
fn find_child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name));
        matches.then_some(path)
    })
}

/// True if `game` is a directory containing an `s/startup-sequence` boot script,
/// i.e. it can boot on its own as a hard drive without the WHDLoad helper.
fn is_self_booting_dir(game: &Path) -> bool {
    find_child_ci(game, "s")
        .is_some_and(|s_dir| find_child_ci(&s_dir, "startup-sequence").is_some())
}

fn create_core(
    system_type: SystemType,
    game: &Path,
    mut settings: HashMap<String, String>,
) -> Result<RetroCore> {
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
    }
    let core = get_core(system_type).unwrap();
    let retro_core = RetroCore::new(Path::new(&core), system_dir(), Some(game), settings)?;
    Ok(retro_core)
}

struct GameInfo {
    title: String,
    group: String,
    year: String,
    system_type: SystemType,
    tags: HashMap<String, String>,
}

fn get_info(game: &Path) -> GameInfo {
    let mut title: String = "".into();
    let mut group: String = "".into();
    let mut year: String = "".into();
    let mut tags = HashMap::new();
    let mut system_type = SystemType::Unknown;
    if let Some(ext) = game.extension()
        && ext == "m3u"
    {
        let m3u = parse_m3u(game).unwrap();
        if let Some(t) = m3u.tags.get("title") {
            title = format!("\"{t}\"");
        }
        if let Some(t) = m3u.tags.get("group") {
            group = t.clone();
        }
        if let Some(t) = m3u.tags.get("year") {
            year = t.clone();
        }
        for (key, val) in m3u.tags {
            if key.starts_with("vice_") || key.starts_with("puae_") {
                warn!("Insert {key} {val}");
                tags.insert(key, val);
            }
        }
        if let Some(path) = m3u.files.first() {
            system_type = get_sytem_type(path);
        }
    } else {
        system_type = get_sytem_type(game);
        title = game.file_name().unwrap().to_string_lossy().to_string();
    }
    GameInfo {
        title,
        group,
        year,
        system_type,
        tags,
    }
}

fn run_retro(
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

    if input.pressed(KeyCode::ControlRight) {
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
            emu.core.set_mouse_buttons(true, false, false);
        }
        if input.just_pressed(KeyCode::KeyC) {
            settings.crt_effect = !settings.crt_effect;
        }
        if input.just_pressed(KeyCode::KeyD) {
            emu.core.next_disk();
        }
        if input.just_pressed(KeyCode::KeyN) {
            emu.run_next = true;
        }
    } else {
        for e in input.get_just_pressed() {
            if let Some(code) = emu.key_map.get(e) {
                emu.core.press_key(*code, true, mods);
            }
        }
        for e in input.get_just_released() {
            if let Some(code) = emu.key_map.get(e) {
                emu.core.press_key(*code, false, mods);
            }
        }

        let motion = mouse_motion.delta;
        if motion != Vec2::ZERO {
            emu.core.add_mouse_motion(motion.x, motion.y);
        }
        emu.core.set_mouse_buttons(
            mouse_buttons.pressed(MouseButton::Left),
            mouse_buttons.pressed(MouseButton::Right),
            mouse_buttons.pressed(MouseButton::Middle),
        );
    }
    if emu.run_next && emu.current_game < emu.games.len() {
        emu.core.unload();
        let game = emu.games[emu.current_game].clone();
        let GameInfo {
            title,
            group,
            year,
            system_type,
            mut tags,
        } = get_info(&game);

        if settings.show_info {
            writer.write(SpawnToast {
                text: format!("{title}\n{group}\n{year}"),
                delay: Duration::from_secs(5),
                duration: Duration::from_secs(15),
            });
        }
        for (key, val) in &emu.tags {
            tags.insert(key.clone(), val.clone());
        }

        if let Ok(work_file) = handle_file(&game, &tags) {
            let core = match create_core(
                work_file.system_type,
                &work_file.path,
                work_file.settings.clone(),
            ) {
                Ok(core) => core,
                Err(e) => {
                    error!("Could not load core for {system_type:?}: {e:#}");
                    return;
                }
            };
            emu.core = core;
            emu.work_file = work_file;
            emu.run_next = false;
            emu.current_game += 1;
            emu.next_frame = time.elapsed_secs_f64();
        }
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

    let frame_time = 1.0 / emu.core.fps();
    // info!(
    //     "FRAME FPS {}/{} t={} AUDIO {}",
    //     fps,
    //     emu.display_fps,
    //     time.delta_secs(),
    //     emu.producer.occupied_len()
    // );
    if emu.producer.occupied_len() > 12000 {
        warn!("Dropping frame");
        emu.next_frame += frame_time;
        return;
    }

    emu.match_fps = false; //(1.0 - emu.display_fps / emu.core.fps()).abs() < 0.02;

    if emu.match_fps {
        //let diff = 7000 - emu.producer.occupied_len();
        emu.core.run();
    } else {
        let t = time.elapsed_secs_f64();
        while t >= emu.next_frame {
            emu.core.run();
            emu.next_frame += frame_time;
        }
    }
    emu.update();

    // For safety
    if emu.producer.occupied_len() < 2000 {
        emu.core.run();
        emu.core.run();
        emu.update();
        warn!("Duplicating frame");
    }
    //}

    //if emu.producer.occupied_len() < 1500 {
    //    emu.core.run();
    //    emu.update();
    //}

    let bg_w = bg.width as usize;
    let bg_h = bg.height as usize;

    emu.core.with_frame(|w, h, frame| {
        let copy_w = w.min(bg_w);
        let copy_h = h.min(bg_h);
        for y in 0..copy_h {
            let src_off = y * w * 4;
            let dst_off = y * bg_w * 4;
            dst[dst_off..dst_off + copy_w * 4]
                .copy_from_slice(&frame[src_off..src_off + copy_w * 4]);
        }
    });
    let aspect = emu.core.aspect_ratio();
    for mut pp in &mut post_process {
        pp.aspect = aspect;
    }

    let (w, h) = emu.core.get_frame_size();
    if w != bg_w || h != bg_h {
        warn!("SIZE CHANGE TO {w} {h}");
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
