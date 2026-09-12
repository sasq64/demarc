use std::time::Duration;

use bevy::window::{Monitor, PrimaryMonitor, PrimaryWindow, WindowMode};
use bevy::{
    asset::RenderAssetUsages,
    camera::visibility::RenderLayers,
    image::Image,
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    render::render_resource::{Extent3d, TextureDimension, TextureFormat},
};

use crate::backend::ViewFocus;
use crate::config::{AppSettings, Args, RenderSettings};
use crate::egui_ui::{HudLocation, HudState, SetHudText};
use crate::emulator::{Emulator, LOAD_SETTLE_SECS, LoadStatus};
use crate::headless::{HeadlessTarget, camera_target};
use crate::mouse_cursor::HideMouse;
use crate::newsys::META_WIDESCREEN;
use crate::post_process::{EmuCamera, PostProcess, ScaleMode, ViewRect};

pub struct FrontendPlugin {}

/// Marks an emulator view as occupying a sub-rectangle of the window,
/// expressed in normalized `[0, 1]` coordinates. [`update_view_rects`] keeps
/// the view's [`ViewRect`] sized to this cell as the window changes.
#[derive(Component, Clone, Copy)]
struct GridCell {
    /// Top-left corner as a fraction of the window size.
    offset: Vec2,
    /// Size as a fraction of the window size.
    size: Vec2,
}

/// Identifies an emulator's on-screen view and its stable index, so the
/// "current" emulator (cycled with RightAlt+Tab) can be looked up and its
/// output area outlined. The rect itself comes from the optional [`GridCell`];
/// a view without one fills the whole window.
#[derive(Component, Clone, Copy)]
struct EmuView {
    index: usize,
}

/// Color of the outline drawn around the currently-focused emulator.
const CURRENT_OUTLINE_COLOR: Color = Color::srgb(1.0, 0.55, 0.0);

/// Build the cells for a `cols`x`rows` grid, laid out left-to-right then
/// top-to-bottom so cell index `i` is the emulator's stable index.
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

fn fix_window(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.mode = WindowMode::Windowed;
}

/// The area views are laid out in: the window, or the offscreen target when
/// `--headless` left us without one.
fn screen_size(window: Option<&Window>, headless: Option<&HeadlessTarget>) -> Option<UVec2> {
    match window {
        Some(window) => Some(window.physical_size()),
        None => headless.map(|h| h.size),
    }
}

/// The OS cursor in physical pixels. Headless there is none.
fn cursor_pos(window: Option<&Window>) -> Option<Vec2> {
    let window = window?;
    Some(window.cursor_position()? * window.scale_factor())
}

fn setup_frontend(world: &mut World) {
    let args = world.resource::<Args>();

    let color_cycle = args.color_cycle;
    let max_time = args.max_time;
    let speed_test = args.speed_test;
    let select = args.select;

    let cells = grid_layout(args);

    let target = camera_target(world.get_resource::<HeadlessTarget>());

    world.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        target,
        EmuCamera,
        RenderLayers::layer(1),
    ));

    if !cells.is_empty() {
        for (i, cell) in cells.into_iter().enumerate() {
            spawn_emulator(world, color_cycle, max_time, speed_test, i, Some(cell));
        }
    } else {
        spawn_emulator(world, color_cycle, max_time, speed_test, 0, None);
        world
            .get_resource_mut::<HideMouse>()
            .map(|mut h| h.0 = true);
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

/// Create a single emulator entity: its own audio stream + ring buffer, its own
/// render-target texture, and the [`PostProcess`] state that samples that
/// texture. Call this once per emulator you want on screen.
///
/// The view lives on the *same* entity as the emulator it shows, so the
/// frontend can walk an emulator and its view with one query.
///
/// `index` is the emulator's stable index — what [`AppSettings::current_emu`]
/// names.
///
/// `cell`, when `Some`, places this emulator in one cell of a grid: it gets a
/// [`GridCell`] marker so [`update_view_rects`] keeps its [`ViewRect`] sized to
/// that cell. Without one the view fills the whole window. The views are *not*
/// cameras — they are all composited by the single [`EmuCamera`] spawned in
/// [`setup_retro`].
fn spawn_emulator(
    world: &mut World,
    color_cycle: bool,
    max_time: Option<usize>,
    speed_test: bool,
    index: usize,
    cell: Option<GridCell>,
) {
    let mut res = world.resource_mut::<Assets<Image>>();
    let emu = Emulator::new(&mut res, max_time, color_cycle, speed_test);
    let handle = emu.image.clone();

    // The view samples this emulator's texture directly and draws it to the
    // screen, letting the post-process shader handle scaling to its rectangle
    // of the window.
    let mut view = world.spawn((
        emu,
        PostProcess {
            source: handle,
            aspect: 0.0, // updated each frame from the core's reported aspect
            aspect_tweak: 1.0,
            used: UVec2::ZERO,
        },
        // The actual rectangle is set from the live window size by
        // `update_view_rects`, before anything reads it.
        ViewRect {
            position: UVec2::ZERO,
            size: UVec2::ZERO,
            active: true,
        },
        EmuView { index },
    ));
    if let Some(cell) = cell {
        // Which fraction of the window this view fills.
        view.insert(cell);
    }
}

/// Keep every emulator view's [`ViewRect`] sized to its slice of the window as
/// the window resizes. Each edge is rounded to a whole pixel; because adjacent
/// cells share an edge fraction they round to the same pixel, so the cells
/// always tile the full window with no gap or overlap.
fn update_view_rects(
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    headless: Option<Res<HeadlessTarget>>,
    mut settings: ResMut<AppSettings>,
    mut views: Query<(&EmuView, Option<&GridCell>, &mut ViewRect)>,
) {
    let window = window.as_deref().copied();
    let Some(size) = screen_size(window, headless.as_deref()) else {
        return;
    };
    if size.x == 0 || size.y == 0 {
        return;
    }

    // Headless the cursor is nowhere, which must not read as the top-left cell.
    let pos = cursor_pos(window).unwrap_or(Vec2::splat(-1.0));
    settings.mouse_index = None;

    let fsize = size.as_vec2();
    for (view, cell, mut rect) in &mut views {
        let Some(cell) = cell else {
            // No grid: this view owns the whole window, always.
            rect.set_if_neq(ViewRect {
                position: UVec2::ZERO,
                size,
                active: true,
            });
            continue;
        };
        // When maximized, the focused emulator fills the whole window and the
        // rest stop drawing, so it looks exactly like it was the only core
        // running.
        let active = !settings.maximized || view.index == settings.current_emu;
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

        // Guarded so we don't retrigger change detection (and a re-extract)
        // every frame when nothing moved.
        rect.set_if_neq(ViewRect {
            position,
            size: vp_size,
            active,
        });
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
        || time.elapsed_secs_f64() - settings.select_box_drawn_at > 2.0
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

/// Wider than this and the screen counts as a widescreen one — 16:10 and 16:9
/// do, 3:2, 4:3 and 5:4 don't.
const WIDE_ASPECT: f32 = 1.55;

/// Tell the loading pipeline what shape the screen is, so a release that has to
/// choose a resolution can choose one that fits it — see [`META_WIDESCREEN`].
///
/// Only when nothing has said already, which is what leaves `-x widescreen=` to
/// whoever typed it.
///
/// The monitor rather than the window, because this is settled once and then
/// kept: on the first frame the window is still the 800x600 winit starts every
/// window as, and latching that shape asked every 16:9 screen for 5:4 modes.
/// A monitor is the right size from the moment it exists, and until one does
/// there is nothing to answer with — so nothing is set and the default stands.
fn detect_widescreen(
    monitors: Query<(&Monitor, Has<PrimaryMonitor>)>,
    headless: Option<Res<HeadlessTarget>>,
    mut settings: ResMut<AppSettings>,
) {
    if settings.system.has_meta(META_WIDESCREEN) {
        return;
    }
    let size = match headless.as_deref() {
        Some(headless) => headless.size,
        // Whichever monitor is the primary one, or the first there is: Wayland
        // has no notion of a primary display, so waiting for one marked that
        // way would be waiting forever.
        None => {
            let monitor = monitors
                .iter()
                .find(|(_, primary)| *primary)
                .or_else(|| monitors.iter().next());
            let Some((monitor, _)) = monitor else {
                return;
            };
            monitor.physical_size()
        }
    };
    if size.x == 0 || size.y == 0 {
        return;
    }
    let wide = size.x as f32 / size.y as f32 >= WIDE_ASPECT;
    debug!("Screen is {}x{}, widescreen={wide}", size.x, size.y);
    settings.system.set_meta(META_WIDESCREEN, wide.to_string());
}

fn handle_loading(
    mut emus: Query<&mut Emulator>,
    mut settings: ResMut<AppSettings>,
    mut writer: MessageWriter<SetHudText>,
    time: Res<Time>,
) {
    let now = time.elapsed_secs_f64();
    for mut emu in &mut emus.iter_mut() {
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
            let over = settings.override_for(&game);
            if let Some(o) = &over {
                debug!("Found override for {game:?}: {o:?}");
            }
            emu.load_async(&game, over.as_ref());
            continue;
        }

        if now >= emu.load_delay_until {
            let status = emu.update_load(&time, &settings.system);
            match status {
                LoadStatus::Idle | LoadStatus::Pending => {}
                LoadStatus::Done {
                    title,
                    result: Err(e),
                } => {
                    let text = format!(
                        "Could not load {title}: {}",
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
                    emu.load_delay_until = now + LOAD_SETTLE_SECS;
                    continue;
                }
                LoadStatus::Done { result: Ok(()), .. } => {
                    emu.run_next = false;
                    emu.run_prev = false;
                    if settings.show_info && settings.maximized {
                        writer.write(SetHudText {
                            text: emu.get_info(),
                            delay: Duration::from_secs(settings.info_delay),
                            duration: Duration::from_secs(settings.info_duration),
                            location: HudLocation::InfoText,
                        });
                    }
                    emu.load_delay_until = now + LOAD_SETTLE_SECS;
                    continue;
                }
            }
        }
    }
}

/// Map the OS cursor to normalized frame coordinates of one view, inverting the
/// same letterbox transform the post-process shader uses
/// (`source_uv = (screen_uv - uv_offset) / uv_scale`). Pointer-driven cores
/// (Flash) need this so the emulator's cursor tracks the visible OS cursor.
///
/// `None` when there is no cursor, the view has no rectangle yet, or the cursor
/// sits outside it — including in the letterbox bars, where it is off-image.
fn cursor_frame_uv(
    pos: Option<Vec2>,
    view_rect: &ViewRect,
    pp: &PostProcess,
    images: &Assets<Image>,
    scale_mode: ScaleMode,
) -> Option<Vec2> {
    let pos = pos?;
    let rect = view_rect.rect()?;
    let vp_min = rect.min.as_vec2();
    let vp_size = rect.size().as_vec2();
    if vp_size.x <= 0.0 || vp_size.y <= 0.0 {
        return None;
    }
    if !Rect::from_corners(vp_min, vp_min + vp_size).contains(pos) {
        return None;
    }
    let screen_uv = (pos - vp_min) / vp_size;
    let src = images
        .get(&pp.source)
        .map(|i| i.size())
        .unwrap_or(UVec2::ONE);
    let (uv_scale, uv_offset) = crate::post_process::view_transform(
        rect.size(),
        src,
        pp.used,
        pp.aspect,
        pp.aspect_tweak,
        scale_mode,
    );
    let frame_uv = (screen_uv - uv_offset) / uv_scale;
    ((0.0..=1.0).contains(&frame_uv.x) && (0.0..=1.0).contains(&frame_uv.y)).then_some(frame_uv)
}

fn run_frontend(
    mut emus: Query<(&mut Emulator, &EmuView, &ViewRect, &mut PostProcess)>,
    input: Res<ButtonInput<KeyCode>>,
    mut settings: ResMut<AppSettings>,
    render: Res<RenderSettings>,
    mouse_buttons: Res<ButtonInput<MouseButton>>,
    mouse_motion: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    mut writer: MessageWriter<SetHudText>,
    mut images: ResMut<Assets<Image>>,
    window: Option<Single<&Window, With<PrimaryWindow>>>,
    headless: Option<Res<HeadlessTarget>>,
    hud: Res<HudState>,
) {
    let cursor = cursor_pos(window.as_deref().copied());
    let mut no_input =
        input.pressed(KeyCode::AltRight) || input.pressed(KeyCode::ControlRight) || hud.modal();
    let mut show_info = false;

    // Handle double click maximize/unmaximize
    if !no_input
        && mouse_buttons.just_pressed(MouseButton::Left)
        && let Some(i) = settings.mouse_index
    {
        no_input = true;
        let t = time.elapsed_secs_f64();
        if t - settings.select_box_drawn_at < 0.35 {
            settings.maximized = !settings.maximized;
        }
        if i < 999 {
            settings.current_emu = i;
            show_info = true;
        }
        settings.select_box_drawn_at = t;
    }

    let now = time.elapsed_secs_f64();

    for (mut emu, view, view_rect, mut pp) in &mut emus {
        let i = view.index;
        if images.get(&emu.image).is_none_or(|i| i.data.is_none()) {
            continue;
        }
        // Drop audio entirely in the speed-test benchmark and when headless.
        emu.audio_active(
            !settings.speed_test
                && headless.is_none()
                && (settings.all_emus || i == settings.current_emu),
        );
        // Exactly one view is focused; the others are on screen as grid tiles
        // unless the focused one is maximized over them.
        emu.focus(match (i == settings.current_emu, settings.maximized) {
            (true, _) => ViewFocus::Focus,
            (false, true) => ViewFocus::Invisible,
            (false, false) => ViewFocus::Visible,
        });

        if show_info && i == settings.current_emu {
            writer.write(SetHudText {
                text: emu.get_info(),
                duration: Duration::from_secs(2),
                location: HudLocation::InfoText,
                ..Default::default()
            });
        }

        if let Some(mt) = emu.max_time
            && now > emu.start_time + (mt as f64)
            && (now - settings.select_box_drawn_at) > 1.0
        {
            emu.start_time = now + 100.0;
            emu.run_next = true;
        };

        // Idle handling
        let mut max_idle = settings.idle_timeout;
        if max_idle == 0 && settings.tv_mode {
            max_idle = 20;
        }
        if max_idle > 0 && emu.idle_time > max_idle as f32 {
            debug!("Idle for {max_idle}, running next");
            emu.run_next = true;
            emu.reset_idle(&time);
        }

        if emu.core.is_none() {
            continue;
        }

        if (settings.all_emus || i == settings.current_emu) && !no_input && settings.maximized {
            let abs = cursor_frame_uv(cursor, view_rect, &pp, &images, render.scale_mode);
            emu.feed_inputs(&input, &mouse_buttons, &mouse_motion, abs);
        }
        emu.run(&time);

        if emu.skip_finished() {
            // Remove warp indicator
            writer.write(SetHudText {
                text: String::new(),
                location: HudLocation::TopRight,
                ..Default::default()
            });
        }

        let bg_w = emu.width as usize;
        let bg_h = emu.height as usize;

        // Only copy (and so re-upload) when the backend has different pixels
        let hash = emu.core.as_ref().unwrap().frame_hash();
        if hash != emu.frame_hash {
            emu.frame_hash = hash;
            if let Some(mut image) = images.get_mut(&emu.image)
                && let Some(dst) = image.data.as_mut()
            {
                emu.core.as_mut().unwrap().with_frame(&mut |w, h, frame| {
                    // The texture is a byte buffer; the frame is one packed RGBA
                    // `u32` per pixel, so copy it through a byte view.
                    let frame = crate::backend::frame_bytes(frame);
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

        let aspect = emu.core.as_mut().unwrap().aspect_ratio();
        if pp.aspect != aspect {
            pp.aspect = aspect;
        }

        let (used_w, used_h) = emu.core.as_mut().unwrap().get_used_frame_size();
        let used = UVec2::new(used_w as u32, used_h as u32);
        if pp.used != used {
            pp.used = used;
        }

        let (w, h) = emu.core.as_mut().unwrap().get_frame_size();

        if (w != bg_w || h != bg_h) && w > 0 && h > 0 {
            debug!("Emulator size changed to {w}x{h}");
            emu.width = w as u32;
            emu.height = h as u32;
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

impl Plugin for FrontendPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_frontend, fix_window, setup_gizmos));
        app.add_systems(
            Update,
            (
                run_frontend,
                detect_widescreen.before(handle_loading),
                handle_loading,
                update_view_rects,
                draw_current_emu_outline,
            ),
        );
    }
}
