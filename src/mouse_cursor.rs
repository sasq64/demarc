//! Hides the OS mouse pointer while an emulator fills the screen.
//!
//! Set [`HideMouse`] for the layouts that fill the window with emulator output
//! and nothing else; the pointer then disappears whenever the window is
//! fullscreen and no mouse-driven UI is up.

use bevy::prelude::*;
use bevy::window::{CursorOptions, Monitor, PrimaryWindow};

use crate::egui_ui::HudState;
use crate::screensaver::is_fullscreen;

pub struct MouseCursorPlugin;

impl Plugin for MouseCursorPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<HideMouse>();
        #[cfg(target_os = "macos")]
        app.init_resource::<mac_cursor::MacCursor>();
        app.add_systems(Update, sync_cursor_visibility);
    }
}

/// Whether the mouse pointer should be hidden while the window is fullscreen.
///
/// Set by `setup_retro` for the layouts that fill the window with emulator
/// output and nothing else. A `--grid` leaves it clear: there the pointer picks
/// which view has focus, so it has to stay visible.
#[derive(Resource, Default)]
pub struct HideMouse(pub bool);

/// Hides the OS pointer over a fullscreen emulator, and brings it back for any
/// UI the user is expected to point at.
///
/// The picker and the settings dialog are both mouse-driven, so [`HudState::modal`]
/// vetoes the hide for as long as either is up.
fn sync_cursor_visibility(
    window: Single<&Window, With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
    mut cursor_options: Single<&mut CursorOptions>,
    hide_mouse: Res<HideMouse>,
    hud: Res<HudState>,
    #[cfg(target_os = "macos")] mut mac_cursor: ResMut<mac_cursor::MacCursor>,
) {
    let hide = hide_mouse.0 && !hud.modal() && is_fullscreen(*window, &monitors);

    cursor_options.visible = !hide;
    #[cfg(target_os = "macos")]
    mac_cursor.set_hidden(hide);
}

/// Hides the OS cursor via Quartz on macOS.
///
/// Bevy/winit's `CursorOptions::visible` maps to `NSCursor hide`/`unhide`,
/// which the window server keeps re-asserting via its cursor-rect mechanism
/// for a borderless-fullscreen `NSWindow` (there's no real fullscreen space to
/// anchor it to), so the arrow reappears the moment the mouse moves. Dropping
/// to `CGDisplayHideCursor`/`CGDisplayShowCursor` hides it at the display
/// level instead, sidestepping that entirely.
#[cfg(target_os = "macos")]
mod mac_cursor {
    use bevy::prelude::*;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGDisplayHideCursor(display: u32) -> i32;
        fn CGDisplayShowCursor(display: u32) -> i32;
    }

    /// Tracks the last state we told Quartz, so repeated calls with the same
    /// value are no-ops. This matters because `CGDisplayHideCursor` /
    /// `CGDisplayShowCursor` are refcounted (per Apple's docs): calling
    /// `Hide` every frame without a balancing `Show` each time would need an
    /// equal number of `Show` calls to ever bring the cursor back.
    #[derive(Resource, Default)]
    pub struct MacCursor {
        hidden: bool,
    }

    impl MacCursor {
        pub fn set_hidden(&mut self, hidden: bool) {
            if hidden == self.hidden {
                return;
            }
            self.hidden = hidden;
            // SAFETY: CGMainDisplayID/CGDisplayHideCursor/CGDisplayShowCursor
            // take no pointers and are safe to call from any thread.
            unsafe {
                let display = CGMainDisplayID();
                if hidden {
                    CGDisplayHideCursor(display);
                } else {
                    CGDisplayShowCursor(display);
                }
            }
        }
    }
}
