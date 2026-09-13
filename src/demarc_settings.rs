//! The app's own settings: the one struct the settings dialog opens (RightAlt+E,
//! [`crate::commands::Cmd::Settings`]) and the system that puts what comes back
//! into effect.
//!
//! Everything about *drawing* a settings dialog is generic and lives in
//! [`crate::egui_settings`]; this module is just its caller, and is the only
//! half that knows what any of these fields mean.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::window::{MonitorSelection, PrimaryWindow, WindowMode};

use crate::config::AppSettings;
use crate::egui_settings::{Range, ReflectDisplay, SettingsApplied};
use crate::egui_ui::SetHudText;
// `wine` is Linux-only and this file is not, so the keys have to be nameable
// everywhere.
#[cfg(target_os = "linux")]
use crate::wine::{META_DIALOG_RES, META_DLL_OVERRIDES, META_RES, PICK};
#[cfg(not(target_os = "linux"))]
const META_RES: &str = "wine_res";
#[cfg(not(target_os = "linux"))]
const META_DIALOG_RES: &str = "wine_dialog_res";
#[cfg(not(target_os = "linux"))]
const META_DLL_OVERRIDES: &str = "wine_dll_overrides";
#[cfg(not(target_os = "linux"))]
const PICK: &str = "pick";

#[derive(Default, Debug, Clone, Copy, PartialEq, Reflect)]
#[reflect(Display)]
pub enum Resolution {
    /// Leave the size to the release: its name, tags or `overrides.toml`.
    #[default]
    Auto,
    Res640x480,
    Res800x600,
    Res1024x768,

    Res1280x720,
    Res1920x1080,
}

impl Resolution {
    /// `WIDTHxHEIGHT`, or `None` for [`Resolution::Auto`].
    pub fn as_meta(&self) -> Option<&'static str> {
        match self {
            Resolution::Auto => None,
            Resolution::Res640x480 => Some("640x480"),
            Resolution::Res800x600 => Some("800x600"),
            Resolution::Res1024x768 => Some("1024x768"),
            Resolution::Res1280x720 => Some("1280x720"),
            Resolution::Res1920x1080 => Some("1920x1080"),
        }
    }
}

impl std::fmt::Display for Resolution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_meta().unwrap_or("Auto"))
    }
}

#[derive(Default, Debug, Clone, PartialEq, Reflect)]
pub struct WineSettings {
    pub resolution: Resolution,
    /// `WINEDLLOVERRIDES`, spelled wine's way. Empty leaves the release's own
    /// overrides in charge.
    pub overrides: String,
    /// Stop at the demo's own setup dialog instead of driving it.
    pub show_startup_dialog: bool,
    /// TBD: nothing reads this yet.
    pub filter: bool,
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

    pub wine: WineSettings,
}

/// Puts an applied [`DemarcSettings`] into effect.
///
/// Field by field against the last applied value, rather than writing all of
/// them: the dialog edits a snapshot, and a field it never touched must not
/// clobber what something else did while it was open -- RightAlt+F moving the
/// window, say.
pub fn apply_settings(
    mut reader: MessageReader<SettingsApplied<DemarcSettings>>,
    mut current: ResMut<DemarcSettings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    mut clear_color: ResMut<ClearColor>,
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
        if new.wine != current.wine {
            apply_wine(&new.wine, &current.wine, app_settings.system.meta_mut());
            hud.write(SetHudText {
                text: "Wine settings from next release".to_owned(),
                duration: std::time::Duration::from_secs(3),
                ..default()
            });
        }
        *current = new.clone();
    }
}

/// Writes the wine fields that changed as run-wide meta. An empty value removes
/// the key, so the release's own meta applies again.
fn apply_wine(new: &WineSettings, old: &WineSettings, meta: &mut HashMap<String, String>) {
    let mut set = |key: &str, value: &str| {
        if value.is_empty() {
            meta.remove(key);
        } else {
            meta.insert(key.to_owned(), value.to_owned());
        }
    };
    if new.resolution != old.resolution {
        set(META_RES, new.resolution.as_meta().unwrap_or(""));
    }
    if new.overrides != old.overrides {
        set(META_DLL_OVERRIDES, new.overrides.trim());
    }
    if new.show_startup_dialog != old.show_startup_dialog {
        set(
            META_DIALOG_RES,
            if new.show_startup_dialog { PICK } else { "" },
        );
    }
}

#[cfg(test)]
#[path = "tests/demarc_settings_tests.rs"]
mod tests;
