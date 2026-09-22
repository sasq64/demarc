//! `--dj-mode`: a second window showing the cross fade emulator.
//!
//! With `--cross-fade` the next release is loaded into a spare emulator that
//! runs off screen and fades itself in ([`crate::cross_fade`]). In DJ mode that
//! spare gets a window of its own — so the next demo can be cued up, watched
//! silently while it boots, and brought over to the main window by hand with
//! RightAlt+Shift+O ([`Cmd::StartOther`](crate::commands::Cmd::StartOther)).
//!
//! The window shows one view: a [`PostProcess`] entity pointed at whichever
//! emulator is currently the spare, composited straight (no CRT effect) by the
//! same pass that draws the main window.
//!
//! Bevy's keyboard and mouse state is one set of resources for the whole app,
//! not one per window, so [`DjWindow::focused`] is what the consumers of it
//! test: while this window has focus the picker is drawn here, per-emulator
//! commands go to the cue, and the main window's view is left alone.
//!
//! The HUD overlay moves here for good: in DJ mode the main window is what the
//! audience sees, so warp indicators, info text and download progress belong on
//! this side of the desk.

use bevy::camera::RenderTarget;
use bevy::ecs::schedule::ScheduleLabel;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::window::WindowRef;
use bevy::{camera::visibility::RenderLayers, prelude::*};
use bevy_egui::{EguiContexts, EguiSchedule};

use crate::config::Args;
use crate::egui_ui::{
    AppFont, FuzzyListSelect, HudState, apply_style, draw_hud, draw_picker, set_scale,
};
use crate::emulator::Emulator;
use crate::post_process::{EmuCamera, PostProcess, ViewRect};

/// Present only in DJ mode, so its absence is what the rest of the app tests.
#[derive(Resource)]
pub struct DjWindow {
    window: Entity,
    /// The camera holding this window's Egui context.
    ui: Entity,
    /// Whether this window, rather than the main one, has the keyboard.
    pub focused: bool,
}

/// Whether the DJ window owns the input this frame.
pub fn has_focus(dj: Option<&DjWindow>) -> bool {
    dj.is_some_and(|dj| dj.focused)
}

/// Marks the camera that draws the DJ window.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct DjCamera;

/// Marks the one view that camera draws.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct DjView;

/// The Egui pass of the DJ window's context.
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct DjContextPass;

const DJ_SIZE: (u32, u32) = (720, 540);

fn setup_dj(mut commands: Commands) {
    let window = commands
        .spawn(Window {
            title: "Demarc DJ".into(),
            resolution: DJ_SIZE.into(),
            ..default()
        })
        .id();
    let target = RenderTarget::Window(WindowRef::Entity(window));

    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        target.clone(),
        EmuCamera,
        DjCamera,
        RenderLayers::layer(1),
    ));
    let ui = commands
        .spawn((
            Camera2d,
            Camera {
                order: 1,
                clear_color: ClearColorConfig::None,
                ..default()
            },
            target,
            RenderLayers::layer(3),
            EguiSchedule::new(DjContextPass),
        ))
        .id();

    // Pointed at the spare by `update_dj_view` on the first frame; until then
    // there is no source image and the view is simply skipped.
    commands.spawn((
        PostProcess {
            source: Handle::default(),
            aspect: 0.0,
            aspect_tweak: 1.0,
            used: UVec2::ZERO,
            view: ViewRect {
                position: UVec2::ZERO,
                size: UVec2::ZERO,
                active: true,
            },
            alpha: 1.0,
            raw: true,
        },
        DjView,
    ));

    commands.insert_resource(DjWindow {
        window,
        ui,
        focused: false,
    });
}

fn track_focus(mut dj: ResMut<DjWindow>, windows: Query<&Window>) {
    if let Ok(window) = windows.get(dj.window) {
        dj.focused = window.focused;
    }
}

/// Follow the spare: it changes entity every time the cross fade hands the main
/// window over, and its picture is what this window is for.
fn update_dj_view(
    dj: Res<DjWindow>,
    windows: Query<&Window>,
    emus: Query<(&Emulator, &PostProcess), Without<DjView>>,
    mut view: Query<&mut PostProcess, With<DjView>>,
) {
    let (Ok(mut pp), Ok(window)) = (view.single_mut(), windows.get(dj.window)) else {
        return;
    };
    pp.view.size = window.physical_size();
    let Some((emu, src)) = emus.iter().find(|(emu, _)| emu.is_crossfade) else {
        return;
    };
    pp.source = emu.image.clone();
    pp.aspect = src.aspect;
    pp.aspect_tweak = src.aspect_tweak;
    pp.used = src.used;
}

/// The DJ window's Egui pass: the HUD overlay, and the file picker while this
/// window has focus — the main window draws the picker the rest of the time.
fn dj_ui(
    mut contexts: EguiContexts,
    dj: Res<DjWindow>,
    windows: Query<&Window>,
    app_font: Res<AppFont>,
    fonts: Res<Assets<Font>>,
    mut state: ResMut<HudState>,
    mut selected: MessageWriter<FuzzyListSelect>,
    keys: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut styled: Local<bool>,
) -> Result {
    let ctx = contexts.ctx_for_entity_mut(dj.ui)?;
    if !*styled {
        let Some(font) = fonts.get(&app_font.0) else {
            return Ok(());
        };
        apply_style(ctx, font);
        *styled = true;
    }
    if let Ok(window) = windows.get(dj.window) {
        set_scale(ctx, window);
    }
    draw_hud(ctx, &state, &time);
    if dj.focused {
        draw_picker(ctx, &keys, &mut state, &mut selected);
    }
    Ok(())
}

pub struct DjPlugin;

impl Plugin for DjPlugin {
    fn build(&self, app: &mut App) {
        let args = app.world().resource::<Args>();
        // No windows at all headless, so nothing to open a second one next to.
        if !args.dj_mode || args.headless {
            return;
        }
        app.add_plugins((
            ExtractComponentPlugin::<DjCamera>::default(),
            ExtractComponentPlugin::<DjView>::default(),
        ))
        .add_systems(Startup, setup_dj)
        .add_systems(PreUpdate, track_focus)
        .add_systems(PostUpdate, update_dj_view)
        .add_systems(DjContextPass, dj_ui);
    }
}
