use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, visibility::RenderLayers},
    image::Image,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    window::WindowMode,
};
use clap::Parser;
use rand::RngExt;

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

use crate::retro_emu::RetroEmu;

pub struct GamePlugin {}

#[derive(Component)]
pub struct Velocity(Vec2);

const LOW_RES_WIDTH: u32 = 384;
const LOW_RES_HEIGHT: u32 = 272;

const CORE_PATH: &str = "vice-libretro/vice_x64_libretro.so";
const SYSTEM_DIR: &str = "system";
const DEFAULT_GAME_PATH: &str = "to_norah.prg";

#[derive(Parser, Debug, Resource, Clone)]
#[command(name = "rupix", about = "Bevy + libretro front-end")]
struct Args {
    /// Path to the program/ROM to load
    #[arg(default_value = DEFAULT_GAME_PATH)]
    game: PathBuf,

    /// How to map the low-res render target onto the window.
    #[arg(long, value_enum, default_value_t = ScaleModeArg::Fit)]
    scale: ScaleModeArg,
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
    emu: RetroEmu,
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
        self.emu.with_audio(|samples| {
            //println!("{}", self.producer.occupied_len());
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

fn setup_cameras(mut commands: Commands, mut images: ResMut<Assets<Image>>, args: Res<Args>) {
    let image = Image::new_target_texture(
        LOW_RES_WIDTH,
        LOW_RES_HEIGHT,
        TextureFormat::bevy_default(),
        None,
    );
    let low_res = images.add(image);

    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        RenderTarget::Image(low_res.clone().into()),
    ));

    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            ..default()
        },
        PostProcess {
            source: low_res,
            scale_mode: args.scale.into(),
        },
        RenderLayers::layer(1),
    ));
}

const CYCLES_PER_FRAME: f64 = 19656.0;
const CLOCK_HZ: f64 = 985248.0;
const SECONDS_PER_FRAME: f64 = CYCLES_PER_FRAME / CLOCK_HZ;

fn setup_retro(world: &mut World) {
    let mut core = RetroEmu::new(Path::new(CORE_PATH), Path::new(SYSTEM_DIR))
        .expect("Failed to load libretro core");

    let (producer, consumer) = ringbuf::HeapRb::<f32>::new(4096 * 8).split();
    let stream = init_audio_stream(48000.0, consumer).unwrap();

    let game_path = world.resource::<Args>().game.clone();
    core.load_game(&game_path).unwrap();
    let width = 384;
    let height = 272;

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

    world.spawn((
        Sprite::from_image(handle.clone()),
        Transform::from_xyz(0.0, 0.0, -1.0),
    ));

    world.insert_resource(Background {
        handle,
        width,
        height,
    });
    let time = world.resource::<Time>();
    world.insert_non_send_resource(Emulator {
        emu: core,
        producer,
        _stream: stream,
        next_frame: time.elapsed_secs_f64() + SECONDS_PER_FRAME,
        key_map: Emulator::build_keycode_map(),
    });
}

fn run_retro(
    mut core: NonSendMut<Emulator>,
    input: Res<ButtonInput<KeyCode>>,
    bg: Res<Background>,
    time: Res<Time>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(image) = images.get_mut(&bg.handle) else {
        return;
    };
    let Some(dst) = image.data.as_mut() else {
        return;
    };

    // info!(
    //     "time {} delta {} next_frame {}",
    //     time.elapsed_secs_f64(),
    //     time.delta_secs_f64(),
    //     core.next_frame
    // );
    for e in input.get_just_pressed() {
        if let Some(code) = core.key_map.get(e) {
            core.emu.press_key(*code, true);
        }
    }
    for e in input.get_just_released() {
        if let Some(code) = core.key_map.get(e) {
            core.emu.press_key(*code, false);
        }
    }

    if time.elapsed_secs_f64() > core.next_frame {
        core.next_frame += SECONDS_PER_FRAME;
        core.emu.run();
        core.update();
    }

    if core.producer.occupied_len() < 4000 {
        core.emu.run();
        core.update();
    }

    let bg_w = bg.width as usize;
    let bg_h = bg.height as usize;

    core.emu.with_frame(|w, h, frame| {
        let copy_w = w.min(bg_w);
        let copy_h = h.min(bg_h);
        for y in 0..copy_h {
            let src_off = y * w * 4;
            let dst_off = y * bg_w * 4;
            dst[dst_off..dst_off + copy_w * 4]
                .copy_from_slice(&frame[src_off..src_off + copy_w * 4]);
        }
    });
}

fn spawn_sprites(asset_server: Res<AssetServer>, mut commands: Commands) {
    let image: Handle<Image> = asset_server.load("face.png");
    let mut rng = rand::rng();

    for _ in 0..1 {
        let v = Vec2::new(rng.random_range(-5.0..5.0), rng.random_range(-5.0..5.0));

        let mut _entity = commands.spawn((
            Transform::from_xyz(0.0, 0.0, 0.0),
            Sprite::from_image(image.clone()),
            Velocity(v),
        ));
    }
}

fn update_sprites(mut sprites: Query<(&mut Transform, &Velocity)>) {
    for (mut tx, vel) in sprites.iter_mut() {
        tx.translation += vel.0.extend(0.0);
        if tx.translation.x > 640.0 {
            tx.translation.x -= 1280.0;
        }
        if tx.translation.y > 3600.0 {
            tx.translation.y -= 720.0;
        }
    }
}

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_cameras, setup_retro));
        app.add_systems(Update, (run_retro, update_sprites));
    }
}

fn main() {
    let args = Args::parse();

    tracing_subscriber::fmt().with_target(true).compact().init();
    let primary_window = Some(Window {
        title: "Rupix".into(),
        mode: WindowMode::BorderlessFullscreen(MonitorSelection::Current),
        //resolution: (384 * 4, 272 * 4).into(),
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
            GamePlugin {},
            PostProcessPlugin,
        ))
        .run();
}
