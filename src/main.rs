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
}

impl Emulator {
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
    });
}

fn run_retro(
    mut core: NonSendMut<Emulator>,
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
