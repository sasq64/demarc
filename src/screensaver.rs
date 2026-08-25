//! Suppresses the system screen saver / display blanking while the emulator is
//! shown fullscreen, so a long demo or attract loop doesn't get blanked out by
//! an idle timer.
//!
//! On Linux this prefers the Wayland `zwp_idle_inhibit_manager_v1` protocol,
//! which is answered by the compositor itself, and falls back to the
//! freedesktop `org.freedesktop.ScreenSaver` D-Bus service (honoured by GNOME,
//! KDE and most other desktops) when we're not on Wayland. On other platforms
//! the plugin compiles to a no-op.

use bevy::prelude::*;
#[cfg(not(target_os = "linux"))]
use bevy::window::WindowMode;
use bevy::window::{CursorOptions, Monitor, PrimaryWindow, RawHandleWrapper, WindowPosition};

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
    window: Single<(&Window, Option<&RawHandleWrapper>), With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
    mut cursor_options: Single<&mut CursorOptions>,
    mut inhibitor: ResMut<ScreenSaverInhibitor>,
    #[cfg(target_os = "macos")] mut mac_cursor: ResMut<mac_cursor::MacCursor>,
) {
    let (window, handle) = window.into_inner();
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

    inhibitor.set_inhibited(fullscreen, handle);
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
pub(crate) fn covers_a_monitor(window: &Window, monitors: &Query<&Monitor>) -> bool {
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
    use bevy::window::RawHandleWrapper;
    use raw_window_handle::RawWindowHandle;

    const APP_NAME: &str = "Demarc";
    const REASON: &str = "Fullscreen emulation";

    /// The freedesktop screen-saver inhibition interface. `Inhibit` returns a
    /// cookie that is later passed to `UnInhibit` to release it.
    ///
    /// Only used off Wayland: it needs a daemon to own the well-known name, and
    /// a compositor-only session may well have none (Hyprland under Omarchy 4
    /// dropped hypridle, which used to provide it, in favour of a quickshell
    /// idle monitor that watches `ext-idle-notify-v1` instead).
    #[zbus::proxy(
        interface = "org.freedesktop.ScreenSaver",
        default_service = "org.freedesktop.ScreenSaver",
        default_path = "/org/freedesktop/ScreenSaver"
    )]
    trait ScreenSaver {
        fn inhibit(&self, application_name: &str, reason_for_inhibit: &str) -> zbus::Result<u32>;
        fn un_inhibit(&self, cookie: u32) -> zbus::Result<()>;
    }

    /// The transport we settled on for this session, opened lazily on the first
    /// inhibition request and reused across fullscreen toggles.
    enum Backend {
        Wayland(wayland::IdleInhibit),
        DBus {
            proxy: ScreenSaverProxyBlocking<'static>,
            cookie: Option<u32>,
        },
    }

    #[derive(Resource, Default)]
    pub struct ScreenSaverInhibitor {
        backend: Option<Backend>,
        /// The state we last *asked* for, tracked separately from the backend's
        /// own state so a failing request isn't retried (and re-logged) every
        /// single frame.
        inhibited: bool,
        /// Cleared on success so a later, genuinely new failure still gets
        /// reported once.
        warned: bool,
        pub hide_mouse: bool,
    }

    impl ScreenSaverInhibitor {
        pub fn set_inhibited(&mut self, inhibited: bool, window: Option<&RawHandleWrapper>) {
            // No window handle means the window isn't up (yet); leave the
            // requested state unlatched so we act on it once one appears,
            // rather than picking a backend blind.
            let Some(window) = window else { return };
            if inhibited == self.inhibited {
                return;
            }
            self.inhibited = inhibited;
            match self.apply(inhibited, window) {
                Ok(()) => self.warned = false,
                Err(err) => {
                    if !self.warned {
                        self.warned = true;
                        warn!("Screen saver inhibition request failed: {err}");
                    }
                }
            }
        }

        fn apply(&mut self, inhibited: bool, window: &RawHandleWrapper) -> anyhow::Result<()> {
            // A window teardown/rebuild (or a compositor restart) invalidates
            // the surface a Wayland inhibitor is anchored to, so drop the
            // backend and rebuild it against the new one.
            if let Some(Backend::Wayland(wl)) = &self.backend
                && !wl.matches(window)
            {
                self.backend = None;
            }
            let backend = match &mut self.backend {
                Some(backend) => backend,
                None => self.backend.insert(Self::connect(window)?),
            };

            match backend {
                Backend::Wayland(wl) => wl.set_inhibited(inhibited),
                Backend::DBus { proxy, cookie } => {
                    if inhibited {
                        *cookie = Some(proxy.inhibit(APP_NAME, REASON)?);
                    } else if let Some(cookie) = cookie.take() {
                        proxy.un_inhibit(cookie)?;
                    }
                    Ok(())
                }
            }
        }

        fn connect(window: &RawHandleWrapper) -> anyhow::Result<Backend> {
            if let RawWindowHandle::Wayland(_) = window.get_window_handle() {
                return Ok(Backend::Wayland(wayland::IdleInhibit::new(window)?));
            }
            let conn = zbus::blocking::Connection::session()?;
            Ok(Backend::DBus {
                proxy: ScreenSaverProxyBlocking::new(&conn)?,
                cookie: None,
            })
        }
    }

    /// Inhibition via `zwp_idle_inhibit_manager_v1`.
    ///
    /// This is the path that works on a bare Wayland session: the request goes
    /// to the compositor, which is also the thing that hands out the idle
    /// notifications a screen locker acts on, so no D-Bus idle daemon has to be
    /// running for it to take effect.
    ///
    /// We can't ask winit for its `wayland-client` objects, so we build a second
    /// `Connection` over the same `wl_display` (libwayland is designed for this:
    /// the extra connection gets its own event queue and never sees winit's
    /// events) and re-wrap the window's `wl_surface` pointer as a proxy on it.
    /// The inhibitor has to name a real, mapped surface — compositors ignore
    /// ones anchored to an unmapped surface — so a throwaway surface of our own
    /// wouldn't do.
    mod wayland {
        use anyhow::{Context, anyhow};
        use bevy::window::RawHandleWrapper;
        use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
        use wayland_client::backend::{Backend, ObjectId};
        use wayland_client::globals::{GlobalListContents, registry_queue_init};
        use wayland_client::protocol::wl_registry::WlRegistry;
        use wayland_client::protocol::wl_surface::WlSurface;
        use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
        use wayland_protocols::wp::idle_inhibit::zv1::client::{
            zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1,
            zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
        };

        /// Dispatch target for our private queue. Every interface we touch is
        /// either event-free or one whose events we don't care about, so this
        /// carries no state.
        struct State;

        impl Dispatch<WlRegistry, GlobalListContents> for State {
            fn event(
                _: &mut Self,
                _: &WlRegistry,
                _: <WlRegistry as Proxy>::Event,
                _: &GlobalListContents,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }

        wayland_client::delegate_noop!(State: ignore ZwpIdleInhibitManagerV1);
        wayland_client::delegate_noop!(State: ignore ZwpIdleInhibitorV1);

        pub struct IdleInhibit {
            queue: EventQueue<State>,
            qh: QueueHandle<State>,
            manager: ZwpIdleInhibitManagerV1,
            surface: WlSurface,
            /// Address of the `wl_surface` `surface` was built from, kept so we
            /// can notice the window being recreated underneath us. Held as an
            /// integer rather than a pointer: it is only ever compared, never
            /// dereferenced, and this keeps the resource `Send + Sync`.
            surface_addr: usize,
            inhibitor: Option<ZwpIdleInhibitorV1>,
        }

        impl IdleInhibit {
            pub fn new(handle: &RawHandleWrapper) -> anyhow::Result<Self> {
                let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) =
                    (handle.get_display_handle(), handle.get_window_handle())
                else {
                    return Err(anyhow!("window is not a Wayland surface"));
                };

                // SAFETY: winit keeps the `wl_display` alive for as long as the
                // window exists, and `from_foreign_display` records that it does
                // not own it, so dropping our `Connection` won't disconnect it.
                let backend =
                    unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
                let conn = Connection::from_backend(backend);
                let (globals, queue) = registry_queue_init::<State>(&conn)
                    .context("no Wayland registry on the window's display")?;
                let qh = queue.handle();
                let manager: ZwpIdleInhibitManagerV1 = globals
                    .bind(&qh, 1..=1, ())
                    .context("compositor does not support zwp_idle_inhibit_manager_v1")?;

                // SAFETY: same lifetime argument as the display; `from_ptr`
                // validates that the proxy really is a `wl_surface`.
                let id = unsafe {
                    ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast())
                }
                .context("window handle is not a wl_surface")?;
                let surface = WlSurface::from_id(&conn, id)
                    .map_err(|_| anyhow!("window's wl_surface is already dead"))?;

                Ok(Self {
                    queue,
                    qh,
                    manager,
                    surface,
                    surface_addr: window.surface.as_ptr() as usize,
                    inhibitor: None,
                })
            }

            /// Whether this inhibitor is still anchored to the window's current
            /// surface.
            pub fn matches(&self, window: &RawHandleWrapper) -> bool {
                matches!(
                    window.get_window_handle(),
                    RawWindowHandle::Wayland(w) if w.surface.as_ptr() as usize == self.surface_addr
                )
            }

            pub fn set_inhibited(&mut self, inhibited: bool) -> anyhow::Result<()> {
                match (inhibited, self.inhibitor.take()) {
                    (true, None) => {
                        self.inhibitor =
                            Some(self.manager.create_inhibitor(&self.surface, &self.qh, ()));
                    }
                    (true, existing @ Some(_)) => self.inhibitor = existing,
                    (false, Some(inhibitor)) => inhibitor.destroy(),
                    (false, None) => return Ok(()),
                }
                // Requests are buffered, so push them out now. A roundtrip also
                // drains our own queue (registry churn we never look at) and
                // surfaces a protocol error here rather than at some unrelated
                // point later.
                self.queue
                    .roundtrip(&mut State)
                    .context("Wayland roundtrip failed")?;
                Ok(())
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod stub {
    use bevy::prelude::*;
    use bevy::window::RawHandleWrapper;

    #[derive(Resource, Default)]
    pub struct ScreenSaverInhibitor {
        pub hide_mouse: bool,
    }

    impl ScreenSaverInhibitor {
        pub fn set_inhibited(&mut self, _inhibited: bool, _window: Option<&RawHandleWrapper>) {}
    }
}
