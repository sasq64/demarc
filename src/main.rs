use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    image::Image,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::WindowMode,
};
use bevy_tweening::lens::TextColorLens;
use bevy_tweening::{CycleCompletedEvent, Delay, Tween, TweenAnim, TweeningPlugin};
use clap::Parser;

use cpal::{
    SampleFormat, SampleRate, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

#[allow(warnings)]
mod libretro;

mod post_process;
mod retro_emu;

use post_process::{PostProcess, PostProcessPlugin, ScaleMode};
use ringbuf::{
    HeapCons, HeapProd,
    traits::{Observer, Split, *},
};

use crate::retro_emu::RetroCore;

pub struct RetroPlugin {}

#[derive(Component)]
pub struct InfoText;

const CORE_PATH_VICE: &str = "vice-libretro/vice_x64_libretro.so";
const CORE_PATH_UAE: &str = "libretro-uae/puae_libretro.so";
const CORE_PATH: &str = "libretro-uae/puae_libretro.so";
const SYSTEM_DIR: &str = "system";

#[derive(Parser, Debug, Resource, Clone)]
#[command(name = "rupix", about = "Bevy + libretro front-end")]
struct Args {
    /// Path to the program/ROM to load
    games: Vec<PathBuf>,

    /// How to map the low-res render target onto the window.
    #[arg(long, value_enum, default_value_t = ScaleModeArg::Fit)]
    scale: ScaleModeArg,

    /// Shuffle the list of games into a random order.
    #[arg(long)]
    shuffle: bool,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum ScaleModeArg {
    /// Fill the window, distorting the aspect ratio.
    Stretch,
    /// Preserve aspect ratio, adding letterbox/pillarbox bars.
    Fit,
    /// Preserve aspect ratio, cropping top/bottom or left/right to fill.
    Zoom,
}

impl From<ScaleModeArg> for ScaleMode {
    fn from(s: ScaleModeArg) -> Self {
        match s {
            ScaleModeArg::Stretch => ScaleMode::Stretch,
            ScaleModeArg::Fit => ScaleMode::Fit,
            ScaleModeArg::Zoom => ScaleMode::Zoom,
        }
    }
}

#[derive(Resource)]
struct Background {
    handle: Handle<Image>,
    width: u32,
    height: u32,
}

struct Emulator {
    core: RetroCore,
    games: Vec<PathBuf>,
    current_game: usize,
    run_next: bool,
    producer: HeapProd<f32>,
    _stream: cpal::Stream,
    next_frame: f64,
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
        self.core.with_audio(|samples| {
            self.producer
                .push_iter(samples.iter().map(|&i| (i as f32) / 32767.0));
        });
    }
}

fn init_audio_stream(core_sample_rate: f64, mut c: HeapCons<f32>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host.default_output_device().unwrap();

    let config = device
        .supported_output_configs()?
        .find(|c| c.sample_format() == SampleFormat::F32)
        .or_else(|| device.supported_output_configs().ok()?.next())
        .expect("no supported config")
        .with_sample_rate(SampleRate(core_sample_rate as u32));

    let mut config: StreamConfig = config.into();
    info!(
        "cpal cfg: rate={} channels={}",
        config.sample_rate.0, config.channels
    );
    config.channels = 2;

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            c.pop_slice(output);
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok(stream)
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

fn setup_cameras(mut commands: Commands, args: Res<Args>, background: Res<Background>) {
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
            scale_mode: args.scale.into(),
            aspect_tweak: 0.9375, // C64 non square pixels
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

#[derive(Message)]
struct SpawnToast {
    text: String,
    delay: Duration,
    duration: Duration,
}

fn spawn_toast(mut commands: Commands, mut reader: MessageReader<SpawnToast>) {
    for msg in reader.read() {
        let tween0 = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_secs(1),
            TextColorLens {
                start: Color::srgba(0., 0., 0., 0.),
                end: Color::WHITE,
            },
        );
        let tween = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_secs(1),
            TextColorLens {
                start: Color::WHITE,
                end: Color::srgba(0., 0., 0., 0.),
            },
        )
        .with_cycle_completed_event(true);

        let delayed = Delay::new(msg.delay)
            .then(tween0)
            .then(Delay::new(msg.duration))
            .then(tween);

        commands.spawn((
            Node {
                //width: Val::Px(400.0),
                position_type: PositionType::Absolute,
                bottom: Val::Px(0.0),
                right: Val::Px(0.0),
                margin: UiRect::all(Val::Px(60.0)),
                ..default()
            },
            Text::new(&msg.text),
            InfoText,
            TextFont {
                font_size: 48.0,
                ..default()
            },
            TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
            TextLayout {
                justify: Justify::Right,
                linebreak: LineBreak::WordBoundary,
            },
            TweenAnim::new(delayed),
        ));
    }
}

fn handle_tween_done(mut commands: Commands, mut reader: MessageReader<CycleCompletedEvent>) {
    for msg in reader.read() {
        info!("DESPAWN");
        commands.entity(msg.anim_entity).despawn();
    }
}

struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SpawnToast>().add_systems(
            Update,
            (
                spawn_toast.run_if(on_message::<SpawnToast>),
                handle_tween_done.run_if(on_message::<CycleCompletedEvent>),
            ),
        );
    }
}

const CYCLES_PER_FRAME: f64 = 19656.0;
const CLOCK_HZ: f64 = 985248.0;
const SECONDS_PER_FRAME: f64 = CYCLES_PER_FRAME / CLOCK_HZ;

fn setup_retro(world: &mut World) {
    let (producer, consumer) = ringbuf::HeapRb::<f32>::new(4096 * 8).split();
    let stream = init_audio_stream(44100.0, consumer).unwrap();

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

    let time = world.resource::<Time>();
    let games = &world.resource::<Args>().games;
    let core = RetroCore::new(Path::new(CORE_PATH), Path::new(SYSTEM_DIR), None, settings)
        .expect("Failed to load libretro core");
    world.insert_non_send_resource(Emulator {
        core,
        producer,
        _stream: stream,
        current_game: 0,
        run_next: !games.is_empty(),
        games: games.clone(),
        next_frame: time.elapsed_secs_f64() + SECONDS_PER_FRAME,
        key_map: Emulator::build_keycode_map(),
    });
}

enum SystemType {
    C64,
    Amiga,
    Unknown,
}

fn get_sytem_type(path: &Path) -> SystemType {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        match ext {
            "adf" => SystemType::Amiga,
            "prg" | "d64" => SystemType::C64,
            _ => SystemType::Unknown,
        }
    } else {
        SystemType::Unknown
    }
}

fn get_core(sytem_type: SystemType) -> PathBuf {
    match sytem_type {
        SystemType::C64 => CORE_PATH_VICE.into(),
        SystemType::Amiga => CORE_PATH_UAE.into(),
        _ => CORE_PATH.into(),
    }
}

fn run_retro(
    mut emu: NonSendMut<Emulator>,
    input: Res<ButtonInput<KeyCode>>,
    mut bg: ResMut<Background>,
    time: Res<Time>,
    mut writer: MessageWriter<SpawnToast>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(image) = images.get_mut(&bg.handle) else {
        return;
    };
    let Some(dst) = image.data.as_mut() else {
        return;
    };
    if input.just_pressed(KeyCode::F11) {
        emu.run_next = true;
    }
    if emu.run_next {
        emu.core.unload();
        let game = emu.games[emu.current_game].clone();
        let mut title: String = "".into();
        let mut group: String = "".into();
        let mut year: String = "".into();
        let mut settings = HashMap::new();
        let mut system_type = SystemType::Unknown;
        if let Some(ext) = game.extension()
            && ext == "m3u"
        {
            let m3u = parse_m3u(&game).unwrap();
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
                if key.starts_with("vice_") {
                    settings.insert(key, val);
                }
            }
            if let Some(path) = m3u.files.first() {
                system_type = get_sytem_type(path);
            }
        } else {
            system_type = get_sytem_type(&game);
            title = game.file_name().unwrap().to_string_lossy().to_string();
        }

        writer.write(SpawnToast {
            text: format!("{title}\n{group}\n{year}"),
            delay: Duration::from_secs(5),
            duration: Duration::from_secs(15),
        });

        emu.core = RetroCore::new(
            Path::new(&get_core(system_type)),
            Path::new(SYSTEM_DIR),
            Some(&game),
            settings,
        )
        .expect("Failed to load libretro core");
        emu.run_next = false;
        emu.current_game += 1;
    }

    // info!(
    //     "time {} delta {} next_frame {}",
    //     time.elapsed_secs_f64(),
    //     time.delta_secs_f64(),
    //     core.next_frame
    // );
    //

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

    if input.just_pressed(KeyCode::F12) {
        emu.core.next_disk();
    }

    // info!(
    //     "FRAME {} {}",
    //     time.delta_secs(),
    //     emu.producer.occupied_len()
    // );
    if time.elapsed_secs_f64() > emu.next_frame {
        //info!("EMULATE");
        emu.next_frame += SECONDS_PER_FRAME;
        emu.core.run();
        emu.update();
    }

    if emu.producer.occupied_len() < 4000 {
        emu.core.run();
        emu.update();
    }

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
        app.add_systems(Startup, (setup_retro, setup_cameras).chain());
        app.add_systems(Update, run_retro);
    }
}

fn main() {
    let mut args = Args::parse();

    if args.shuffle {
        use rand::seq::SliceRandom;
        args.games.shuffle(&mut rand::rng());
    }

    tracing_subscriber::fmt().with_target(true).compact().init();
    let primary_window = Some(Window {
        title: "Rupix".into(),
        //mode: WindowMode::BorderlessFullscreen(MonitorSelection::Current),
        resolution: (384 * 3, 288 * 3).into(),
        resizable: false,
        ..Default::default()
    });

    App::new()
        .insert_resource(args)
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window,
                ..Default::default()
            }),
            RetroPlugin {},
            PostProcessPlugin,
            TweeningPlugin,
            HudPlugin,
        ))
        .run();
}
