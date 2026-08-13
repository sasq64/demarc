use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{
    asset::RenderAssetUsages,
    camera::Viewport,
    camera::visibility::RenderLayers,
    image::Image,
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

use crate::commands::{CmdMessage, check_hotkey};
use crate::emulator::Emulator;
use crate::fuzzy_list::FuzzyListSelect;
use crate::hud::{HudLocation, SetHudText, TextList};
use crate::post_process::PostProcess;
use crate::screensaver::ScreenSaverInhibitor;
use crate::systems::{SystemType, get_info_text, tags_from_args};
use crate::text_input::TextInput;
use crate::{AppSettings, Args, RenderSettings};

pub struct RetroPlugin {}

const SYSTEM_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/system.zip"));

/// SHA-256 of `system.zip`, computed by `build.rs` when the archive is packed.
const SYSTEM_CHECKSUM: &str = env!("SYSTEM_ZIP_CHECKSUM");

pub fn system_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let system = resolve_system_dir();
        system
            .canonicalize()
            .unwrap_or_else(|e| panic!("Failed to canonicalize system dir {system:?}: {e}"))
    })
    .as_path()
}

/// Locate the `system` directory, preferring a local one in debug builds and
/// otherwise extracting the embedded `system.zip` into the user cache.
fn resolve_system_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        let local = PathBuf::from("system");
        if local.is_dir() {
            debug!("Using local system dir");
            return local;
        }
        warn!("Could not find local system dir");
    }
    let cache = dirs::cache_dir().unwrap_or_default().join("demarc");
    info!("CACHE {cache:?}");
    let system = cache.join("system");
    let checksum_file = system.join(".checksum");
    let up_to_date = std::fs::read_to_string(&checksum_file)
        .map(|c| c.trim() == SYSTEM_CHECKSUM)
        .unwrap_or(false);
    if !up_to_date {
        std::fs::create_dir_all(&cache).expect("Failed to create demarc cache directory");
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(SYSTEM_ZIP))
            .expect("Failed to read embedded system.zip");
        archive
            .extract(&cache)
            .expect("Failed to extract system.zip");
        std::fs::write(&checksum_file, SYSTEM_CHECKSUM).expect("Failed to write system checksum");
    }
    system
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

fn setup_ui_camera(mut commands: Commands, args: Res<Args>, asset_server: Res<AssetServer>) {
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
    commands.spawn((
        Node {
            display: Display::None,
            position_type: PositionType::Absolute,
            bottom: px(25.0),
            left: px(15.0),
            ..default()
        },
        TextInput {
            // The app font, not Bevy's ASCII-only built-in one, so typed
            // non-ASCII characters actually have glyphs.
            font: asset_server.load("font.ttf"),
            ..default()
        },
    ));
}

fn fix_window(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.mode = WindowMode::Windowed;
}

fn setup_retro(world: &mut World) {
    let args = world.resource::<Args>();

    let mut tags = tags_from_args(args);

    let match_fps = args.force_vsync;
    let max_time = args.max_time;
    let speed_test = args.speed_test;
    let select = args.select;

    let cells = grid_layout(args);

    if cells.is_empty() {
        spawn_emulator(world, tags, match_fps, max_time, speed_test, None);
        world.resource_mut::<ScreenSaverInhibitor>().hide_mouse = true;
    } else {
        // pcsx rearmed can only run 3 instances at one time
        tags.insert("psx_core".into(), "beetle".into());
        for (i, cell) in cells.into_iter().enumerate() {
            spawn_emulator(
                world,
                tags.clone(),
                match_fps,
                max_time,
                speed_test,
                Some((i, cell)),
            );
        }
    }

    // With `--select` the user picks a file from the selector before anything
    // runs, so clear the default `run_next` that would otherwise auto-load the
    // first file. `open_select_menu` opens the selector on the first frame.
    if select {
        let mut emus = world.query::<&mut Emulator>();
        for mut emu in emus.iter_mut(world) {
            emu.run_next = false;
        }
    }
}

fn handle_textlist(
    mut commands: Commands,
    mut settings: ResMut<AppSettings>,
    mut reader: MessageReader<FuzzyListSelect>,
) {
    for &FuzzyListSelect { id, item, .. } in reader.read() {
        if id == 1 {
            println!("START {item}");
            if let Some(e) = settings.file_list.take() {
                commands.entity(e).despawn();
            }
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
    match_fps: bool,
    max_time: Option<usize>,
    speed_test: bool,
    cell: Option<(usize, GridCell)>,
) {
    let mut res = world.resource_mut::<Assets<Image>>();
    let emu = Emulator::new(&mut res, tags, max_time, match_fps, speed_test);
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
    mut settings: ResMut<AppSettings>,
    mut cameras: Query<(&GridCell, &EmuView, &mut Camera)>,
) {
    let size = window.physical_size();
    if size.x == 0 || size.y == 0 {
        return;
    }

    let pos = window.cursor_position().unwrap_or_default() * window.scale_factor();
    settings.mouse_index = None;

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

        if !settings.maximized {
            let p0 = cell.offset * fsize;
            let p1 = (cell.offset + cell.size) * fsize;
            let r = Rect::from_corners(p0, p1);
            if r.contains(pos) {
                settings.mouse_index = Some(view.index);
            }
        } else {
            settings.mouse_index = Some(99999);
        }

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

fn run_retro(
    mut emus: Query<&mut Emulator>,
    input: Res<ButtonInput<KeyCode>>,
    lists: Query<&TextList>,
    mut settings: ResMut<AppSettings>,
    render: Res<RenderSettings>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mouse_motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    mut writer: MessageWriter<SetHudText>,
    mut cmd_writer: MessageWriter<CmdMessage>,
    mut images: ResMut<Assets<Image>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut cameras: Query<(&EmuView, &Camera, &mut PostProcess)>,
) {
    // A controlled TextList is capturing keyboard navigation; while it is open,
    // swallow all keys so they don't also reach the emulated machine.
    let modal = lists.iter().any(|l| l.controlled);
    let cmd = if !modal {
        let hot_key = input.pressed(KeyCode::AltRight) || input.pressed(KeyCode::ControlRight);
        if hot_key {
            settings.last_draw = time.elapsed_secs_f64();
        }
        if hot_key { check_hotkey(&input) } else { None }
    } else {
        None
    };

    let mut show_info = false;
    if mouse_buttons.just_pressed(MouseButton::Left)
        && let Some(i) = settings.mouse_index
    {
        let t = time.elapsed_secs_f64();
        if t - settings.last_draw < 0.2 {
            settings.maximized = !settings.maximized;
        }
        if i < 999 {
            settings.current_emu = i;
            show_info = true;
        }
        settings.last_draw = t;
    }

    if let Some(cmd) = cmd {
        settings.hotkey_pressed = 0.0;
        cmd_writer.write(CmdMessage(cmd));
    }

    // Map the OS cursor to normalized frame coordinates of the emulator it is
    // over, inverting the same letterbox transform the post-process shader uses
    // (`source_uv = (screen_uv - uv_offset) / uv_scale`). Pointer-driven cores
    // (Flash) need this so the emulator's cursor tracks the visible OS cursor.
    let cursor = window.cursor_position().map(|c| c * window.scale_factor());
    let mut pointer: Option<(usize, Vec2)> = None;
    if let Some(pos) = cursor {
        for (view, camera, pp) in &cameras {
            if !camera.is_active {
                continue;
            }
            let Some(rect) = camera.physical_viewport_rect() else {
                continue;
            };
            let vp_min = rect.min.as_vec2();
            let vp_size = rect.size().as_vec2();
            if vp_size.x <= 0.0 || vp_size.y <= 0.0 {
                continue;
            }
            if !Rect::from_corners(vp_min, vp_min + vp_size).contains(pos) {
                continue;
            }
            let screen_uv = (pos - vp_min) / vp_size;
            let src = images
                .get(&pp.source)
                .map(|i| i.size())
                .unwrap_or(UVec2::ONE);
            let (uv_scale, uv_offset) = crate::post_process::scale_offset(
                rect.size(),
                src,
                pp.aspect,
                pp.aspect_tweak,
                render.scale_mode,
            );
            let frame_uv = (screen_uv - uv_offset) / uv_scale;
            // Ignore hits in the letterbox bars, where the cursor is off-image.
            if (0.0..=1.0).contains(&frame_uv.x) && (0.0..=1.0).contains(&frame_uv.y) {
                pointer = Some((view.index, frame_uv));
                break;
            }
        }
    }

    for (i, mut emu) in &mut emus.iter_mut().enumerate() {
        // Read-only probe. The mutable borrow that the frame copy needs is taken
        // further down, only when the core has something new: `get_mut` marks the
        // asset modified when it drops, and Bevy answers that by re-uploading the
        // whole texture.
        if images.get(&emu.image).is_none_or(|i| i.data.is_none()) {
            continue;
        }
        // Drop audio entirely in the speed-test benchmark.
        emu.audio_active(!settings.speed_test && (settings.all_emus || i == settings.current_emu));

        let flen = settings.files.len() as isize;

        let d = if emu.run_next && (settings.tv_mode || settings.current_game < flen - 1) {
            1
        } else if emu.run_prev && (settings.tv_mode || settings.current_game > 0) {
            -1
        } else {
            0
        };
        if d != 0 {
            settings.current_game = (settings.current_game + d + flen) % flen;
            let game = settings.files[settings.current_game as usize].clone();
            match emu.load(&time, &game) {
                Err(e) => {
                    let text = format!(
                        "Could not load {}: {}",
                        game.game_info.title,
                        crate::load_error::classify(&e).reason()
                    );

                    if !settings.tv_mode {
                        emu.run_next = false;
                        emu.run_prev = false;
                        writer.write(SetHudText {
                            text,
                            delay: Duration::from_secs(0),
                            duration: Duration::from_secs(4),
                            location: HudLocation::Error,
                        });
                    }
                    error!("{e:?}");
                }
                Ok(()) => {
                    emu.run_next = false;
                    emu.run_prev = false;
                    if settings.show_info && settings.maximized {
                        // writer.write(SetHudText {
                        //     text: get_info_text(&emu.work_file),
                        //     delay: Duration::from_secs(settings.info_delay),
                        //     duration: Duration::from_secs(settings.info_duration),
                        //     location: HudLocation::InfoText,
                        // });
                    }
                }
            };
            continue;
        }

        if show_info && i == settings.current_emu {
            // writer.write(SetHudText {
            //     text: get_info_text(&emu.work_file),
            //     duration: Duration::from_secs(2),
            //     location: HudLocation::InfoText,
            //     ..Default::default()
            // });
        }

        let et = time.elapsed_secs_f64();
        if let Some(mt) = emu.max_time
            && et > emu.start_time + (mt as f64)
            && (et - settings.last_draw) > 1.0
        {
            emu.run_next = true;
        };

        let mut max_idle = settings.idle_timeout;
        if max_idle == 0 && settings.tv_mode {
            max_idle = 20;
            // if emu.work_file.system_type == SystemType::Ilbm
            //     || emu.work_file.system_type == SystemType::Gfx
            // {
            //     max_idle = 10;
            // }
        }

        if max_idle > 0 && emu.idle_time > max_idle as f32 {
            emu.run_next = true;
        }

        if emu.core.is_none() {
            continue;
        }

        if (settings.all_emus || i == settings.current_emu) && cmd.is_none() && !modal {
            let abs = pointer.and_then(|(idx, p)| (idx == i).then_some(p));
            emu.feed_inputs(&input, &mouse_buttons, &mouse_motion, abs);
        }
        emu.run(&time);

        let bg_w = emu.width as usize;
        let bg_h = emu.height as usize;

        // Only copy (and so re-upload) when the backend has different pixels
        // than the last copy. The screen refreshes at 60-165Hz while a core
        // produces 50-60 frames a second — and the threaded backend often has no
        // update ready at all — so most passes through here have nothing new.
        let hash = emu.core.as_ref().unwrap().frame_hash();
        if hash != emu.frame_hash {
            emu.frame_hash = hash;
            // Scoped so the `AssetMut` (whose destructor fires change detection)
            // releases the `images` borrow before it is taken again below.
            if let Some(mut image) = images.get_mut(&emu.image)
                && let Some(dst) = image.data.as_mut()
            {
                emu.core.as_mut().unwrap().with_frame(&mut |w, h, frame| {
                    // The texture is a byte buffer; the frame is one packed RGBA
                    // `u32` per pixel, so copy it through a byte view.
                    let frame = crate::retro_emu::frame_bytes(frame);
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
        }
        // For some reason we need to compensate the hatari aspect
        let aspect = if false {
            // emu.work_file.system_type == SystemType::AtariST {
            let (w, h) = emu.core.as_mut().unwrap().get_frame_size();
            if h > 0 {
                w as f32 / h as f32
            } else {
                emu.core.as_mut().unwrap().aspect_ratio()
            }
        } else {
            emu.core.as_mut().unwrap().aspect_ratio()
        };

        // Guarded: `PostProcess` is extracted into the render world, and the
        // aspect only moves when the core changes video mode.
        for (_, _, mut pp) in &mut cameras {
            if pp.source == emu.image && pp.aspect != aspect {
                pp.aspect = aspect;
            }
        }

        let (w, h) = emu.core.as_mut().unwrap().get_frame_size();

        if (w != bg_w || h != bg_h) && w > 0 && h > 0 {
            debug!("SIZE CHANGE TO {w} {h}");
            emu.width = w as u32;
            emu.height = h as u32;
            // The texture below is replaced with a blank one, so whatever was
            // copied in for this hash is gone: forget it, or a backend that
            // isn't producing new frames (a still image, a paused core) would
            // never refill it and stay black.
            emu.frame_hash = 0;
            if let Some(mut image) = images.get_mut(&emu.image) {
                // Recreate with new dimensions
                *image = Image::new(
                    Extent3d {
                        width: w as u32,
                        height: h as u32,
                        depth_or_array_layers: 1,
                    },
                    TextureDimension::D2,
                    vec![0u8; w * h * 4],
                    // Raw display-space frame, not sRGB — see the note at the
                    // initial texture creation in `emulator.rs`.
                    TextureFormat::Rgba8Unorm,
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
            (
                run_retro,
                update_grid_viewports,
                draw_current_emu_outline,
                handle_textlist,
            ),
        );
    }
}
