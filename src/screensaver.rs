//! Suppresses the system screen saver / display blanking while the emulator is
//! shown fullscreen, so a long demo or attract loop doesn't get blanked out by
//! an idle timer.
//!
//! On Linux this talks to the freedesktop `org.freedesktop.ScreenSaver` D-Bus
//! service (honoured by GNOME, KDE and most other desktops) via zbus. On other
//! platforms the plugin compiles to a no-op.

use bevy::prelude::*;
#[cfg(not(target_os = "linux"))]
use bevy::window::WindowMode;
use bevy::window::{CursorOptions, Monitor, PrimaryWindow, WindowPosition};

pub struct ScreenSaverPlugin;

impl Plugin for ScreenSaverPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ScreenSaverInhibitor>();
        #[cfg(target_os = "macos")]
        app.init_resource::<mac_cursor::MacCursor>();
        app.add_systems(Update, sync_screen_saver);
    }
}

/// Inhibits while the window is fullscreen, releases otherwise.
/// [`ScreenSaverInhibitor::set_inhibited`] is idempotent, so calling it every
/// frame only produces D-Bus traffic on an actual state change.
fn sync_screen_saver(
    window: Single<&Window, With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
    mut cursor_options: Single<&mut CursorOptions>,
    mut inhibitor: ResMut<ScreenSaverInhibitor>,
    #[cfg(target_os = "macos")] mut mac_cursor: ResMut<mac_cursor::MacCursor>,
) {
    let window = window.into_inner();
    // `window.mode` is the primary signal on macOS, where `covers_a_monitor`
    // never matches (a fullscreen NSWindow is sized to the visible frame, not
    // the monitor's physical bounds). On Linux it's unreliable: under Wayland
    // winit leaves `window.mode` at a stale `BorderlessFullscreen` after the
    // window is toggled back out of fullscreen, which would keep us inhibited
    // forever — so there we trust geometric coverage only, as before.
    #[cfg(not(target_os = "linux"))]
    let requested_fullscreen = matches!(
        window.mode,
        WindowMode::BorderlessFullscreen(_) | WindowMode::Fullscreen(_, _)
    );
    #[cfg(target_os = "linux")]
    let requested_fullscreen = false;
    let fullscreen = requested_fullscreen || covers_a_monitor(window, &monitors);
    let hide_cursor = inhibitor.hide_mouse && fullscreen;

    cursor_options.visible = !hide_cursor;
    #[cfg(target_os = "macos")]
    mac_cursor.set_hidden(hide_cursor);

    inhibitor.set_inhibited(fullscreen);
}

/// Fallback fullscreen detection for when [`Window::mode`] doesn't reflect
/// reality.
///
/// [`sync_screen_saver`] checks `window.mode` first since that's the mode we
/// ourselves requested. This exists for the case a compositor (notably
/// Wayland tiling WMs like Hyprland) fullscreens a window on its own,
/// leaving `window.mode` at [`WindowMode::Windowed`]. We catch that by
/// checking whether the window fully covers one of the monitors.
///
/// Note this is a poor fit for macOS: a `BorderlessFullscreen` window there
/// is sized to the screen's *visible* frame (screen minus menu bar), never
/// the monitor's full physical bounds, so this always reports `false` there
/// — harmless since `window.mode` already covers that case correctly.
///
/// On X11 winit reports the window's physical position, so we can do a proper
/// rectangle-cover test. On Wayland the position is never reported (it stays
/// [`WindowPosition::Automatic`]), so we fall back to an exact size match
/// against a monitor — which is what a fullscreened window produces.
fn covers_a_monitor(window: &Window, monitors: &Query<&Monitor>) -> bool {
    let win_w = window.physical_width();
    let win_h = window.physical_height();
    if win_w == 0 || win_h == 0 {
        return false;
    }
    let win_pos = match window.position {
        WindowPosition::At(pos) => Some(pos),
        _ => None,
    };
    monitors.iter().any(|monitor| match win_pos {
        Some(pos) => {
            pos.x <= monitor.physical_position.x
                && pos.y <= monitor.physical_position.y
                && pos.x + win_w as i32
                    >= monitor.physical_position.x + monitor.physical_width as i32
                && pos.y + win_h as i32
                    >= monitor.physical_position.y + monitor.physical_height as i32
        }
        None => win_w == monitor.physical_width && win_h == monitor.physical_height,
    })
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

#[cfg(target_os = "linux")]
pub use linux::ScreenSaverInhibitor;
#[cfg(not(target_os = "linux"))]
pub use stub::ScreenSaverInhibitor;

#[cfg(target_os = "linux")]
mod linux {
    use bevy::prelude::*;

    /// The freedesktop screen-saver inhibition interface. `Inhibit` returns a
    /// cookie that is later passed to `UnInhibit` to release it.
    #[zbus::proxy(
        interface = "org.freedesktop.ScreenSaver",
        default_service = "org.freedesktop.ScreenSaver",
        default_path = "/org/freedesktop/ScreenSaver"
    )]
    trait ScreenSaver {
        fn inhibit(&self, application_name: &str, reason_for_inhibit: &str) -> zbus::Result<u32>;
        fn un_inhibit(&self, cookie: u32) -> zbus::Result<()>;
    }

    /// Holds a lazily-opened D-Bus connection and the active inhibition cookie
    /// (if any). The connection is reused across fullscreen toggles.
    #[derive(Resource, Default)]
    pub struct ScreenSaverInhibitor {
        proxy: Option<ScreenSaverProxyBlocking<'static>>,
        cookie: Option<u32>,
        pub hide_mouse: bool,
    }

    impl ScreenSaverInhibitor {
        pub fn set_inhibited(&mut self, inhibited: bool) {
            if inhibited == self.cookie.is_some() {
                return;
            }
            if let Err(err) = self.apply(inhibited) {
                warn!("Screen saver inhibition request failed: {err}");
            }
        }

        fn apply(&mut self, inhibited: bool) -> zbus::Result<()> {
            if self.proxy.is_none() {
                let conn = zbus::blocking::Connection::session()?;
                self.proxy = Some(ScreenSaverProxyBlocking::new(&conn)?);
            }
            let proxy = self.proxy.as_ref().expect("proxy was just created");
            if inhibited {
                self.cookie = Some(proxy.inhibit("Demarc", "Fullscreen emulation")?);
            } else if let Some(cookie) = self.cookie.take() {
                proxy.un_inhibit(cookie)?;
            }
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod stub {
    use bevy::prelude::*;

    #[derive(Resource, Default)]
    pub struct ScreenSaverInhibitor {
        pub hide_mouse: bool,
    }

    impl ScreenSaverInhibitor {
        pub fn set_inhibited(&mut self, _inhibited: bool) {}
    }
}
