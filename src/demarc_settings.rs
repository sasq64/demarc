//! The app's own settings: the one struct the settings dialog opens (RightAlt+E,
//! [`crate::commands::Cmd::Settings`]) and the system that puts what comes back
//! into effect.
//!
//! Everything about *drawing* a settings dialog is generic and lives in
//! [`crate::egui_settings`]; this module is just its caller, and is the only
//! half that knows what any of these fields mean.

use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};

use crate::config::{AppSettings, RenderSettings, ShaderArg};
use crate::egui_settings::{Range, SettingsApplied};
use crate::egui_ui::SetHudText;
use crate::post_process::ShaderPath;

#[derive(Default, Debug, Clone, Copy, Reflect)]
pub enum Resolution {
    Res640x480,
    #[default]
    Res800x600,
    Res1024x768,

    Res1280x720,
    Res1920x1080,
}

/// The settings the dialog edits.
///
/// The resource is seeded from the command line in `main` and thereafter holds
/// what was last applied, which is both what a fresh open shows and the
/// baseline [`apply_settings`] compares against.
#[derive(Resource, Reflect, Clone, Debug, Default)]
pub struct DemarcSettings {
    pub fullscreen: bool,
    pub background: Color,

    pub fast_load: bool,
    pub resolution: Resolution,

    pub shader: ShaderArg,
    /// Frames a core's worker thread may run ahead. Takes effect on the next
    /// release loaded -- see [`crate::newsys::NewSys::set_meta`]. `0` would be
    /// a rendezvous channel (the worker blocked until the frontend takes each
    /// frame), so the range starts at 1.
    #[reflect(@Range::new(1, 8))]
    pub latency: u32,
    /// TBD: nothing reads this yet. Per-emulator gain already exists
    /// (`AppSettings::audio_gain`); what is missing is a master volume for it
    /// to scale.
    #[reflect(@Range::new(0.0, 100.0))]
    pub volume: f32,
}

/// Puts an applied [`DemarcSettings`] into effect.
///
/// Field by field against the last applied value, rather than writing all of
/// them: the dialog edits a snapshot, and a field it never touched must not
/// clobber what something else did while it was open -- RightAlt+F moving the
/// window, or a `--slangp` preset that no [`ShaderArg`] names.
pub fn apply_settings(
    mut reader: MessageReader<SettingsApplied<DemarcSettings>>,
    mut current: ResMut<DemarcSettings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut clear_color: ResMut<ClearColor>,
    mut shader_path: ResMut<ShaderPath>,
    mut render: ResMut<RenderSettings>,
    mut app_settings: ResMut<AppSettings>,
    mut hud: MessageWriter<SetHudText>,
) {
    for SettingsApplied(new) in reader.read() {
        if new.fullscreen != current.fullscreen {
            window.mode = if new.fullscreen {
                WindowMode::BorderlessFullscreen(MonitorSelection::Current)
            } else {
                WindowMode::Windowed
            };
        }
        if new.background != current.background {
            clear_color.0 = new.background;
        }
        if new.latency != current.latency {
            app_settings
                .system
                .set_meta("latency", new.latency.to_string());
            // The only change here with nothing to see, and it doesn't take
            // hold until the next release, so say so.
            hud.write(SetHudText {
                text: format!("Latency {} from next release", new.latency),
                duration: std::time::Duration::from_secs(3),
                ..default()
            });
        }
        *current = new.clone();
    }
}

#[cfg(test)]
#[path = "tests/demarc_settings_tests.rs"]
mod tests;
