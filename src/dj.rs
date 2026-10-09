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
//! test: while this window has focus per-emulator commands go to the cue, and
//! the main window's view is left alone.
//!
//! The app's one Egui context lives here instead of on the main window: in DJ
//! mode the main window is what the audience sees, so the HUD, the picker and
//! the dialogs belong on this side of the desk.

use bevy::camera::RenderTarget;
use bevy::render::extract_component::{ExtractComponent, ExtractComponentPlugin};
use bevy::window::WindowRef;
use bevy::{camera::visibility::RenderLayers, prelude::*};
use bevy_egui::{EguiGlobalSettings, PrimaryEguiContext, input::FocusedNonWindowEguiContext};

use crate::config::Args;
use crate::emulator::Emulator;
use crate::post_process::{EmuCamera, PostProcess, ViewRect};

/// Present only in DJ mode, so its absence is what the rest of the app tests.
#[derive(Resource)]
pub struct DjWindow {
    window: Entity,
    /// Whether this window, rather than the main one, has the keyboard.
    pub focused: bool,
}

/// Whether the DJ window owns the input this frame.
pub fn has_focus(dj: Option<&DjWindow>) -> bool {
    dj.is_some_and(|dj| dj.focused)
}

/// Whether `--dj-mode` opens its window: there is none to open it next to
/// headless.
pub fn enabled(args: &Args) -> bool {
    args.dj_mode && !args.headless
}

/// Marks the camera that draws the DJ window.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct DjCamera;

/// Marks the one view that camera draws.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct DjView;

const DJ_SIZE: (u32, u32) = (720, 540);

fn setup_dj(mut commands: Commands, mut egui: ResMut<EguiGlobalSettings>) {
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
            PrimaryEguiContext,
        ))
        .id();
    // Egui only hears the keyboard of its own window, and the picker has to
    // work from the main one too.
    egui.enable_focused_non_window_context_updates = false;
    commands.insert_resource(FocusedNonWindowEguiContext(ui));

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
            no_crt: false,
        },
        DjView,
    ));

    commands.insert_resource(DjWindow {
        window,
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

pub struct DjPlugin;

impl Plugin for DjPlugin {
    fn build(&self, app: &mut App) {
        if !enabled(app.world().resource::<Args>()) {
            return;
        }
        app.add_plugins((
            ExtractComponentPlugin::<DjCamera>::default(),
            ExtractComponentPlugin::<DjView>::default(),
        ))
        .add_systems(Startup, setup_dj)
        .add_systems(PreUpdate, track_focus)
        .add_systems(PostUpdate, update_dj_view);
    }
}
