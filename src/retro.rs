use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{
    asset::RenderAssetUsages,
    camera::Viewport,
    camera::visibility::RenderLayers,
    image::Image,
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
    render::view::screenshot::{Screenshot, save_to_disk},
};

use ringbuf::traits::Split;

use crate::audio::{AudioResampler, init_audio_stream};
use crate::emulator::Emulator;
use crate::hud::{SpawnToast, ToastType};
use crate::post_process::{BorderMode, PostProcess, ScaleMode};
use crate::retro_emu::{RetroCoreThreaded, RetroEmu};
use crate::utils::{GameInfo, SystemType, WorkingFile};
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

/// Keeps the per-emulator `cpal` output streams alive. A stream stops playing
/// as soon as it is dropped, and `cpal::Stream` is neither `Send` nor `Sync`
/// (so it can't live in an [`Emulator`] component), so the streams are parked
/// here in a `NonSend` resource for the lifetime of the app. We never touch
/// them again after creation — audio flows through the per-emulator ring buffer.
#[derive(Default)]
struct AudioStreams(Vec<cpal::Stream>);

/// Marks a [`PostProcess`] camera as occupying one quadrant of a 2x2 grid.
/// The index is `0`=top-left, `1`=top-right, `2`=bottom-left, `3`=bottom-right.
/// [`update_grid_viewports`] keeps the camera's viewport sized to its quadrant
/// as the window changes.
#[derive(Component, Clone, Copy)]
struct GridQuadrant(u8);

fn setup_ui_camera(mut commands: Commands) {
    // Camera for full res UI on top of screen. Its order must stay above every
    // emulator camera (grid mode uses orders 0..=3) so the HUD draws on top of
    // all quadrants rather than being overdrawn by a later one.
    commands.spawn((
        Camera2d,
        Camera {
            order: 10,
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
    world.insert_non_send_resource(AudioStreams::default());

    let args = world.resource::<Args>();

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

    let match_fps = args.force_vsync;
    let max_time = args.max_time;
    let grid = args.grid;

    if grid {
        for quadrant in 0..4 {
            spawn_emulator(
                world,
                tags.clone(),
                match_fps,
                max_time,
                Some(quadrant as u8),
            );
        }
    } else {
        spawn_emulator(world, tags, match_fps, max_time, None);
    }
}

/// Split `items` into `n` contiguous parts of as-equal-as-possible length. The
/// first `len % n` parts get one extra element so every item is placed and the
/// original order is preserved.
fn split_into_parts<T>(items: Vec<T>, n: usize) -> Vec<Vec<T>> {
    let len = items.len();
    let base = len / n;
    let rem = len % n;
    let mut iter = items.into_iter();
    (0..n)
        .map(|i| {
            let take = base + if i < rem { 1 } else { 0 };
            iter.by_ref().take(take).collect()
        })
        .collect()
}

/// Create a single emulator entity: its own audio stream + ring buffer, its own
/// render-target texture, and a [`PostProcess`] camera that samples that
/// texture. Call this once per emulator you want on screen.
///
/// `quadrant`, when `Some`, places this emulator in one cell of a 2x2 grid: the
/// camera gets a distinct render order and a [`GridQuadrant`] marker so
/// [`update_grid_viewports`] keeps its viewport sized to that cell.
fn spawn_emulator(
    world: &mut World,
    tags: HashMap<String, String>,
    match_fps: bool,
    max_time: Option<usize>,
    quadrant: Option<u8>,
) {
    let (producer, consumer) = ringbuf::HeapRb::<f32>::new(4096 * 8).split();
    let (sample_rate, stream) = init_audio_stream(consumer).unwrap();
    // Park the stream so it keeps playing; see [`AudioStreams`].
    world.non_send_resource_mut::<AudioStreams>().0.push(stream);

    let mut res = world.resource_mut::<Assets<Image>>();
    let x = &mut (*res);
    //    let handle = x.add(image);
    //   info!("SPWAWN {:?}", handle);
    let emu = Emulator::new(x, tags, max_time, sample_rate, producer);
    let handle = emu.image.clone();
    world.spawn(emu);

    // Samples this emulator's texture directly and renders it to the screen,
    // letting the post-process shader handle scaling to the window. When
    // showing several emulators at once, give each camera a distinct `order`
    // and a `viewport` so they don't overdraw each other.
    let mut camera = world.spawn((
        Camera2d,
        Camera {
            // Distinct order per grid cell so the cameras share one window
            // target cleanly (the lowest-order one clears it once per frame).
            order: quadrant.map_or(0, |q| q as isize),
            ..default()
        },
        PostProcess {
            source: handle,
            aspect: 0.0, // updated each frame from the core's reported aspect
            aspect_tweak: 1.0,
        },
        RenderLayers::layer(1),
    ));
    if let Some(q) = quadrant {
        // The actual viewport rectangle is set from the live window size by
        // `update_grid_viewports`; this just tags which quadrant to fill.
        camera.insert(GridQuadrant(q));
    }
}

/// Keep each [`GridQuadrant`] camera's viewport sized to its cell of the 2x2
/// grid as the window resizes. The right/bottom cells absorb any odd remainder
/// so the quadrants always tile the full window with no gap.
fn update_grid_viewports(
    window: Single<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&GridQuadrant, &mut Camera)>,
) {
    let size = window.physical_size();
    if size.x == 0 || size.y == 0 {
        return;
    }
    let half = size / 2;
    for (quadrant, mut camera) in &mut cameras {
        let q = quadrant.0;
        let left = q % 2 == 0;
        let top = q < 2;
        let position = UVec2::new(if left { 0 } else { half.x }, if top { 0 } else { half.y });
        let vp_size = UVec2::new(
            if left { half.x } else { size.x - half.x },
            if top { half.y } else { size.y - half.y },
        );
        // `Viewport` doesn't derive `PartialEq`; compare the fields we set to
        // avoid retriggering change detection (and a render-graph rebuild)
        // every frame when nothing moved.
        let unchanged = camera
            .viewport
            .as_ref()
            .is_some_and(|v| v.physical_position == position && v.physical_size == vp_size);
        if !unchanged {
            camera.viewport = Some(Viewport {
                physical_position: position,
                physical_size: vp_size,
                ..default()
            });
        }
    }
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
}

pub fn get_core(sytem_type: SystemType) -> Result<PathBuf, &'static str> {
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

pub fn create_core(
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
    mut emus: Query<&mut Emulator>,
    input: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<AppSettings>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mouse_motion: Res<AccumulatedMouseMotion>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    time: Res<Time>,
    mut writer: MessageWriter<SpawnToast>,
    mut images: ResMut<Assets<Image>>,
    mut post_process: Query<&mut PostProcess>,
) {
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
    }

    for mut emu in &mut emus {
        let Some(image) = images.get_mut(&emu.image) else {
            continue;
        };
        let Some(dst) = image.data.as_mut() else {
            continue;
        };

        if emu.run_next && settings.current_game < settings.games.len() {
            emu.run_next = false;
            let game = settings.games[settings.current_game].clone();
            emu.load(&time, &game);
            settings.current_game += 1;
            if settings.show_info {
                let GameInfo { title, group, year } = &emu.work_file.game_info;
                writer.write(SpawnToast {
                    text: format!("\"{title}\"\n{group}\n{year}"),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(5000),
                    toast_type: ToastType::InfoText,
                });
            }
            continue;
        }

        if let Some(mt) = emu.max_time
            && time.elapsed_secs_f64() > emu.start_time + (mt as f64)
        {
            emu.run_next = true;
        };

        if input.pressed(KeyCode::AltRight) {
            if input.just_pressed(KeyCode::KeyM) {
                emu.set_mouse_buttons(true, false, false);
            }
            if input.just_pressed(KeyCode::KeyC) {
                settings.crt_effect = !settings.crt_effect;
            }
            if input.just_pressed(KeyCode::KeyD) {
                emu.disk_no = (emu.disk_no + 1) % emu.get_number_of_disks();
                let disk_no = emu.disk_no;
                emu.set_disk(disk_no);
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
                emu.reset();
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
            // if input.just_pressed(KeyCode::KeyW) {
            //     for _ in 0..500 {
            //         core.run();
            //         core.with_audio(&mut |_| {});
            //     }
            // }
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
            emu.feed_inputs(&input, &mouse_buttons, &mouse_motion);
        };
        emu.run(&time);

        let bg_w = emu.width as usize;
        let bg_h = emu.height as usize;

        emu.core.as_mut().unwrap().with_frame(&mut |w, h, frame| {
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
            let (w, h) = emu.core.as_mut().unwrap().get_frame_size();
            if h > 0 {
                w as f32 / h as f32
            } else {
                emu.core.as_mut().unwrap().aspect_ratio()
            }
        } else {
            emu.core.as_mut().unwrap().aspect_ratio()
        };
        // Update only this emulator's own post-process camera (the one
        // sampling its texture), so multiple emulators don't clobber each
        // other's aspect ratio.
        for mut pp in &mut post_process {
            if pp.source == emu.image {
                pp.aspect = aspect;
            }
        }

        let (w, h) = emu.core.as_mut().unwrap().get_frame_size();

        if w != bg_w || h != bg_h {
            debug!("SIZE CHANGE TO {w} {h}");
            emu.width = w as u32;
            emu.height = h as u32;
            if let Some(image) = images.get_mut(&emu.image) {
                // Recreate with new dimensions
                *image = Image::new(
                    Extent3d {
                        width: w as u32,
                        height: h as u32,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    vec![0u8; w * h * 4],
                    TextureFormat::Rgba8UnormSrgb,
                    RenderAssetUsages::default(),
                );
            }
        }
    }
}

impl Plugin for RetroPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_retro, setup_ui_camera, fix_window));
        app.add_systems(Update, (run_retro, update_grid_viewports));
    }
}
