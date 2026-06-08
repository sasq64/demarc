use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};

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

use crate::emulator::{Emulator, InputMode};
use crate::hud::{HudLocation, SetHudText, TextList};
use crate::post_process::{BorderMode, PostProcess, ScaleMode};
use crate::retro_emu::{RetroCoreThreaded, RetroEmu};
use crate::utils::{GameInfo, SystemType};
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

pub fn system_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if cfg!(debug_assertions) {
            let local = PathBuf::from("system");
            if local.is_dir() {
                return local;
            }
        }
        let cache = dirs::cache_dir().unwrap_or_default().join("demarc");
        info!("CACHE {cache:?}");
        let system = cache.join("system");
        if !system.exists() || !system.join(".v1").exists() {
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

/// Marks a [`PostProcess`] camera as occupying a sub-rectangle of the window,
/// expressed in normalized `[0, 1]` coordinates.
/// [`update_grid_viewports`] keeps the camera's viewport sized to
/// this cell as the window changes.
#[derive(Component, Clone, Copy)]
struct GridCell {
    /// Top-left corner as a fraction of the window size.
    offset: Vec2,
    /// Size as a fraction of the window size.
    size: Vec2,
}

/// Identifies an emulator's on-screen camera and its stable index, so the
/// "current" emulator (cycled with RightAlt+Tab) can be looked up and its
/// output area outlined. The rect itself comes from the optional [`GridCell`];
/// a camera without one fills the whole window.
#[derive(Component, Clone, Copy)]
struct EmuView {
    index: usize,
}

/// Color of the outline drawn around the currently-focused emulator.
const CURRENT_OUTLINE_COLOR: Color = Color::srgb(1.0, 0.55, 0.0);

/// Build the cells for a `cols`x`rows` grid, laid out left-to-right then
/// top-to-bottom so cell index `i` maps cleanly to a distinct camera order.
fn grid_cells(cols: u32, rows: u32) -> Vec<GridCell> {
    let mut cells = Vec::with_capacity((cols * rows) as usize);
    for row in 0..rows {
        for col in 0..cols {
            cells.push(GridCell {
                offset: Vec2::new(col as f32 / cols as f32, row as f32 / rows as f32),
                size: Vec2::new(1.0 / cols as f32, 1.0 / rows as f32),
            });
        }
    }
    cells
}

fn grid_layout(args: &Args) -> Vec<GridCell> {
    if let Some((cols, rows)) = args.grid {
        grid_cells(cols, rows)
    } else {
        Vec::new()
    }
}

fn setup_ui_camera(mut commands: Commands, args: Res<Args>) {
    // Camera for full res UI on top of screen. Its order must stay above every
    // emulator camera (grid mode gives each cell a distinct order, `0..n`) so
    // the HUD and the focus outline draw on top of all cells rather than being
    // overdrawn by a later one. Derive the order from the cell count instead of
    // a fixed value, which would otherwise be exceeded by grids larger than the
    // constant (e.g. a 4x4 grid hides the outline for cells with order >= it).
    let order = grid_layout(&args).len().max(1) as isize;
    commands.spawn((
        Camera2d,
        Camera {
            order,
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
    // let asset_server = world.resource::<AssetServer>();
    // let font: Handle<Font> = asset_server.load("font.ttf");
    // let mut commands = world.commands();
    // TextList::spawn(
    //     &mut commands,
    //     font,
    //     ["One".into(), "Two".into(), "Three".into()].into(),
    //     3,
    // );

    let args = world.resource::<Args>();
    let mut tags = HashMap::new();
    let mut set_var = |name: &str, val: &str| tags.insert(name.into(), val.into());

    set_var("latency", &args.latency.to_string());

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

    if args.fast_load {
        set_var("vice_jiffydos", "enabled");
    }

    for opt in &args.extra_options {
        if let Some((key, val)) = opt.split_once("=") {
            set_var(key.trim(), val.trim());
        }
    }

    let match_fps = args.force_vsync;
    let max_time = args.max_time;

    let cells = grid_layout(args);

    if cells.is_empty() {
        spawn_emulator(world, tags, match_fps, max_time, None);
    } else {
        for (i, cell) in cells.into_iter().enumerate() {
            spawn_emulator(world, tags.clone(), match_fps, max_time, Some((i, cell)));
        }
    }
}

/// Create a single emulator entity: its own audio stream + ring buffer, its own
/// render-target texture, and a [`PostProcess`] camera that samples that
/// texture. Call this once per emulator you want on screen.
///
/// `cell`, when `Some`, places this emulator in one cell of a grid: the camera
/// gets a distinct render order (from the cell index) and a [`GridCell`] marker
/// so [`update_grid_viewports`] keeps its viewport sized to that cell.
fn spawn_emulator(
    world: &mut World,
    tags: HashMap<String, String>,
    _match_fps: bool,
    max_time: Option<usize>,
    cell: Option<(usize, GridCell)>,
) {
    let mut res = world.resource_mut::<Assets<Image>>();
    let emu = Emulator::new(&mut res, tags, max_time);
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
            order: cell.map_or(0, |(i, _)| i as isize),
            ..default()
        },
        PostProcess {
            source: handle,
            aspect: 0.0, // updated each frame from the core's reported aspect
            aspect_tweak: 1.0,
        },
        RenderLayers::layer(1),
        EmuView {
            index: cell.map_or(0, |(i, _)| i),
        },
    ));
    if let Some((_, cell)) = cell {
        // The actual viewport rectangle is set from the live window size by
        // `update_grid_viewports`; this just tags which cell to fill.
        camera.insert(cell);
    }
}

/// Keep each [`GridCell`] camera's viewport sized to its cell as the window
/// resizes. Each edge is rounded to a whole pixel; because adjacent cells share
/// an edge fraction they round to the same pixel, so the cells always tile the
/// full window with no gap or overlap.
fn update_grid_viewports(
    window: Single<&Window, With<PrimaryWindow>>,
    settings: Res<AppSettings>,
    mut cameras: Query<(&GridCell, &EmuView, &mut Camera)>,
) {
    let size = window.physical_size();
    if size.x == 0 || size.y == 0 {
        return;
    }
    let fsize = size.as_vec2();
    for (cell, view, mut camera) in &mut cameras {
        // When maximized, the focused emulator fills the whole window and the
        // rest stop rendering, so it looks exactly like it was the only core
        // running. Guard the writes so we don't retrigger change detection (and
        // a render-graph rebuild) every frame when nothing changed.
        let is_focused = view.index == settings.current_emu;
        let active = !settings.maximized || is_focused;
        if camera.is_active != active {
            camera.is_active = active;
        }
        if !active {
            continue;
        }
        let (position, vp_size) = if settings.maximized {
            (UVec2::ZERO, size)
        } else {
            let position = (cell.offset * fsize).round().as_uvec2();
            let far = ((cell.offset + cell.size) * fsize).round().as_uvec2();
            (position, far - position)
        };
        // `Viewport` doesn't derive `PartialEq`; compare the fields we set to
        // avoid retriggering change detection every frame when nothing moved.
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

/// Route the default gizmos onto the UI render layer (layer 2) so they draw
/// through the full-res UI camera, on top of every emulator camera.
fn setup_gizmos(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    config.render_layers = RenderLayers::layer(2);
    config.line.width = config_line_width();
}

/// Draw an orange rectangle around the output area of the currently-focused
/// emulator (see [`AppSettings::current_emu`], cycled with RightAlt+Tab). The
/// outline is skipped when only one emulator is on screen, where it would just
/// frame the whole window.
fn draw_current_emu_outline(
    mut gizmos: Gizmos,
    settings: Res<AppSettings>,
    time: Res<Time>,
    window: Single<&Window, With<PrimaryWindow>>,
    views: Query<(&EmuView, Option<&GridCell>)>,
) {
    // A single (or maximized) emulator fills the window, so an outline would
    // just frame the whole screen — not useful.
    if settings.maximized
        || views.iter().count() < 2
        || time.elapsed_secs_f64() - settings.last_draw > 2.0
    {
        return;
    }
    let (w, h) = (window.width(), window.height());
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    if settings.all_emus {
        // Frame the whole screen rather than a single cell.
        let rect = Vec2::new(
            (w - config_line_width()).max(0.0),
            (h - config_line_width()).max(0.0),
        );
        gizmos.rect_2d(Isometry2d::IDENTITY, rect, CURRENT_OUTLINE_COLOR);
        return;
    }
    for (view, cell) in &views {
        if view.index != settings.current_emu {
            continue;
        }
        let (offset, size) = cell.map_or((Vec2::ZERO, Vec2::ONE), |c| (c.offset, c.size));
        // The default Camera2d uses logical pixels with the origin centered and
        // y pointing up; cell offsets are top-left fractions with y down.
        let center = Vec2::new(
            (offset.x + size.x * 0.5 - 0.5) * w,
            (0.5 - (offset.y + size.y * 0.5)) * h,
        );
        // Inset by the line width so the outline sits inside the cell instead
        // of being clipped against the window/cell edges.
        let rect = Vec2::new(
            (size.x * w - config_line_width()).max(0.0),
            (size.y * h - config_line_width()).max(0.0),
        );
        gizmos.rect_2d(
            Isometry2d::from_translation(center),
            rect,
            CURRENT_OUTLINE_COLOR,
        );
    }
}

/// Line width used both for the gizmo config and the outline inset.
const fn config_line_width() -> f32 {
    4.0
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
        set_var("vice_sid_extra", "none");
        set_var("vice_sid_model", "8580");
        set_var("vice_sound_sample_rate", "44100");
    } else if system_type == SystemType::Amstrad {
        set_var("cap32_statusbar", "disabled");
    } else if system_type == SystemType::AtariST {
        set_var("hatari_forcerefresh", "2");
        set_var("hatari_start_in_mouse_mode", "false");
        set_var("hatari_fastboot", "true");
        set_var("hatari_video_crop_overscan", "false");
    }
    match get_core(system_type) {
        Ok(core) => RetroCoreThreaded::new(Path::new(&core), system_dir(), Some(game), settings),
        Err(name) => {
            bail!(
                "Can not find core '{name}' for '{game:?}'.\nExpected in current directory or /usr/lib/libretro"
            );
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
    mut writer: MessageWriter<SetHudText>,
    mut images: ResMut<Assets<Image>>,
    mut post_process: Query<&mut PostProcess>,
) {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    let hot_key = input.pressed(KeyCode::AltRight) || input.pressed(KeyCode::ControlRight);
    let mut show_info = false;
    if hot_key {
        settings.last_draw = time.elapsed_secs_f64();
        if input.just_pressed(KeyCode::KeyC) {
            settings.crt_effect = !settings.crt_effect;
            writer.write(SetHudText {
                text: (if settings.crt_effect {
                    "Filter on"
                } else {
                    "Filter off"
                })
                .into(),
                delay: Duration::from_secs(0),
                duration: Duration::from_secs(1),
                location: HudLocation::TopLeft,
            });
        }
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
            };
            writer.write(SetHudText {
                text: format!("{:?}", settings.scale_mode),
                delay: Duration::from_secs(0),
                duration: Duration::from_secs(1),
                location: HudLocation::TopLeft,
            });
        }
        if input.just_pressed(KeyCode::KeyF) {
            window.mode = match window.mode {
                WindowMode::Windowed => WindowMode::BorderlessFullscreen(MonitorSelection::Current),
                _ => WindowMode::Windowed,
            };
        }
        if emus.count() > 1 {
            if input.just_pressed(KeyCode::KeyA) {
                settings.all_emus = !settings.all_emus;
            }
            if input.just_pressed(KeyCode::Tab) {
                let count = emus.iter().count();
                if count > 0 {
                    if settings.show_info {
                        show_info = true;
                    }
                    if shift {
                        settings.current_emu = (settings.current_emu + count - 1) % count;
                    } else {
                        settings.current_emu = (settings.current_emu + 1) % count;
                    }
                }
            }
            if input.just_pressed(KeyCode::Enter) {
                settings.maximized = !settings.maximized;
                if settings.show_info && settings.maximized {
                    show_info = true;
                }
                if !settings.maximized {
                    writer.write(SetHudText {
                        location: HudLocation::InfoText,
                        ..Default::default()
                    });
                }
            }
        }
    }

    for (i, mut emu) in &mut emus.iter_mut().enumerate() {
        let Some(image) = images.get_mut(&emu.image) else {
            continue;
        };
        let Some(dst) = image.data.as_mut() else {
            continue;
        };
        if show_info && i == settings.current_emu {
            let GameInfo { title, group, year } = &emu.work_file.game_info;
            writer.write(SetHudText {
                text: format!("\"{title}\"\n{group}\n{year}"),
                duration: Duration::from_secs(2),
                location: HudLocation::InfoText,
                ..Default::default()
            });
        }

        emu.audio_active(settings.all_emus || i == settings.current_emu);

        if emu.run_next && settings.current_game < settings.games.len() {
            emu.run_next = false;
            let game = settings.games[settings.current_game].clone();
            emu.load(&time, &game);
            settings.current_game += 1;
            if settings.show_info && settings.maximized {
                let GameInfo { title, group, year } = &emu.work_file.game_info;
                writer.write(SetHudText {
                    text: format!("\"{title}\"\n{group}\n{year}"),
                    delay: Duration::from_secs(8),
                    duration: Duration::from_secs(15),
                    location: HudLocation::InfoText,
                });
            }
            continue;
        }

        if let Some(mt) = emu.max_time
            && time.elapsed_secs_f64() > emu.start_time + (mt as f64)
        {
            emu.run_next = true;
        };

        if emu.core.is_none() {
            continue;
        }

        if settings.all_emus || i == settings.current_emu {
            if hot_key {
                if input.just_pressed(KeyCode::KeyM) {
                    emu.set_mouse_buttons(true, false, false);
                }
                if input.just_pressed(KeyCode::KeyJ) {
                    emu.input_mode = emu.input_mode.next();
                    let text = match emu.input_mode {
                        InputMode::Keyboard => "\u{f030c}",
                        InputMode::Joystick1 => "\u{f0297} #1",
                        InputMode::Joystick2 => "\u{f0297} #2",
                    };
                    writer.write(SetHudText {
                        text: text.into(),
                        delay: Duration::from_secs(0),
                        duration: Duration::from_secs(1),
                        location: HudLocation::BottomLeft,
                    });
                }
                if input.just_pressed(KeyCode::KeyP) {
                    emu.paused = !emu.paused;
                    if emu.paused {
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            duration: Duration::from_secs(1500),
                            text: "\u{f03e4}".into(),
                            ..Default::default()
                        });
                    } else {
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            ..Default::default()
                        });
                    }
                }
                if input.just_pressed(KeyCode::KeyD) {
                    let nd = emu.get_number_of_disks();
                    if nd > 0 {
                        emu.disk_no = (emu.disk_no + 1) % nd;
                    }
                    let disk_no = emu.disk_no;
                    emu.set_disk(disk_no);
                    let floppy = emu.work_file.system_type == SystemType::C64;
                    let d = emu.disk_no + 1;

                    writer.write(SetHudText {
                        location: HudLocation::BottomLeft,
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
                        writer.write(SetHudText {
                            location: HudLocation::InfoText,
                            ..Default::default()
                        });
                    } else {
                        writer.write(SetHudText {
                            text: format!("\"{title}\"\n{group}\n{year}"),
                            delay: Duration::from_secs(0),
                            duration: Duration::from_secs(5000),
                            location: HudLocation::InfoText,
                        });
                    }
                    emu.show_info = !emu.show_info;
                }
                if input.just_pressed(KeyCode::KeyN) {
                    emu.run_next = true;
                    info!("{} vs {}", settings.current_game, settings.games.len());
                }
                if input.just_pressed(KeyCode::KeyW) {
                    let (frames, text) = if shift {
                        (30 * 50, "\u{f0d06}".to_string())
                    } else {
                        (10 * 50, "\u{f0d71}".to_string())
                    };
                    emu.skip(frames);
                    writer.write(SetHudText {
                        location: HudLocation::TopRight,
                        duration: Duration::from_secs(1500),
                        text,
                        ..Default::default()
                    });
                }
                if input.just_pressed(KeyCode::KeyT) {
                    let name = format!(
                        "{}-{}.png",
                        emu.work_file.game_info.title,
                        time.elapsed_secs() as i32
                    );
                    screenshot(&mut commands, &name);
                    writer.write(SetHudText {
                        text: format!("Screenshot: {name}"),
                        delay: Duration::from_secs(0),
                        duration: Duration::from_secs(5000),
                        location: HudLocation::TopLeft,
                    });
                }
            } else {
                emu.feed_inputs(&input, &mouse_buttons, &mouse_motion);
            }
        }
        if !emu.run(&time) {
            writer.write(SetHudText {
                location: HudLocation::TopRight,
                ..Default::default()
            });
        }

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

        if (w != bg_w || h != bg_h) && w > 0 && h > 0 {
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
        app.add_systems(
            Startup,
            (setup_retro, setup_ui_camera, fix_window, setup_gizmos),
        );
        app.add_systems(
            Update,
            (run_retro, update_grid_viewports, draw_current_emu_outline),
        );
    }
}
