//! How a Windows release is started under [wine]: the command, the prefix it
//! runs in, and shutting that prefix down afterwards.
//!
//! Nothing here runs a demo. The running is the gamescope libretro core's
//! (`external/gamescope/src/libretro/`, and `docs/GAMESCOPE.md`): a patched
//! gamescope composites a headless Wayland/Xwayland session into a shared
//! dmabuf and hands the frames back, so a Windows demo is an ordinary picture
//! source like any other backend — shaders, the grid and screenshots all apply
//! to it. What the core cannot work out for itself is *what to run*, and that
//! is what this module is: [`wine_command`] builds the argv,
//! [`crate::newsys::windows`] restates it as the core's options.
//!
//! The command is one line:
//!
//! ```text
//! WINEPREFIX=~/.wine-demarc wine demarc-autodlg.exe --launch demo.exe --prefer 800x600
//! ```
//!
//! Two pieces of that are not obvious:
//!
//! - Nearly every PC demo opens with a setup dialog, and nobody is sitting there
//!   to answer it. `demarc-autodlg.exe` (built from `tools/autodlg`) answers it
//!   through Win32 messages — picking the resolution demarc asked for and
//!   pressing Start/Go/Run — then starts the demo itself. It launches the demo
//!   rather than running beside it because the driver has to be in the same
//!   session as the dialog for `EnumWindows` to see it, which a child is and a
//!   sibling started separately is not.
//! - The driver is in the command even when there is no dialog to answer
//!   (`wine_res=pick`, where `--no-go` has it press nothing), because answering
//!   dialogs is only half of what it does. The other half is saying when the
//!   demo starts and when it ends, which nothing outside the session can see:
//!   wine's services outlive the demo inside it, so the process that was
//!   started stays alive long after the picture has gone. The driver holds the
//!   demo's own handle and writes a line at each end; the core reads them.
//! - `wine_dll_overrides` is `WINEDLLOVERRIDES`, spelled wine's way and passed
//!   through unread. Unset, [`crate::newsys::windows`] writes one from the DLLs
//!   the release itself ships next to its executable: a demo that carries its
//!   own `d3dx9_37.dll` needs that build of it and not wine's reimplementation.
//! - `wine_gl_compat=true` asks Mesa for a compatibility profile even when the
//!   demo asked for a core one, because a core context is missing the extension
//!   strings wine's `wglGetProcAddress` gates the legacy aliases on — see
//!   [`META_GL_COMPAT`].
//! - `wine_sandbox` is on unless an entry turns it off, and puts the whole
//!   command inside a `bwrap` that gives the session a throwaway copy of the
//!   prefix and a pid namespace of its own — so a demo cannot damage what
//!   `just wine-prefix` installed, and wine's services cannot outlive it. See
//!   [`crate::wine_sandbox`].
//! - `wine_desktop=true` puts the pair inside a wine virtual desktop
//!   (`explorer /desktop=`) fixed at the session size. Demos switch display
//!   modes on their way to fullscreen, and under gamescope's Xwayland that
//!   means tearing down and remapping an X window, which a handful of them —
//!   Equinox's *Kings of the Playground* among them — do not survive. Inside a
//!   virtual desktop the mode switch is wine's own business and never reaches
//!   X. It is off by default: the desktop is a window manager of wine's own
//!   between the demo and the screen, and most demos are better off without one.
//!
//! [wine]: https://www.winehq.org

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::system_dir;

/// Meta key holding the resolution to run at, as `WIDTHxHEIGHT`.
pub const META_RES: &str = "wine_res";

/// What that resolution is when nothing says otherwise. 800x600 is the size the
/// setup dialogs of the era all offer, which matters: the driver picks the mode
/// by matching the label on a radio button or combo box entry, so a size no
/// dialog lists is a size no demo will run at.
pub const DEFAULT_RES: &str = "800x600";

/// The `wine_res` value that means "leave the dialog to me".
pub const PICK: &str = "pick";

/// Meta key asking for the demo to be run inside a wine virtual desktop.
pub const META_DESKTOP: &str = "wine_desktop";

/// Meta key asking Mesa for a compatibility profile whatever the demo requested.
///
/// A GL demo of the 2010s asks for a 3.x context and, as the spec says it may,
/// leaves `WGL_CONTEXT_PROFILE_MASK_ARB` out. The default is *core*, so that is
/// what it gets — and a core context does not advertise `GL_ARB_multitexture`,
/// `GL_EXT_draw_range_elements` or the rest of the pre-3.0 extension strings,
/// because their functionality has been core for years.
///
/// On Windows nobody notices. An ICD's `wglGetProcAddress` is a name lookup, so
/// `glActiveTextureARB` comes back as a pointer to `glActiveTexture` no matter
/// which profile is current. Wine's is stricter and checks that the extension
/// the name belongs to is on the current context first, so the same call returns
/// NULL — and a 64k intro, which resolves its GL entry points once into a table
/// and never checks one, calls straight through it. Approximate's *Gaia Machina*
/// dies exactly that way, on `glActiveTextureARB(GL_TEXTURE6)` during FBO setup.
///
/// Asking Mesa for a compatibility context puts the legacy strings back, wine's
/// check passes, and the aliases resolve to the functions they always aliased.
/// Nothing else about the demo changes: it is the same GL either way.
pub const META_GL_COMPAT: &str = "wine_gl_compat";

/// Meta key holding wine's `WINEDLLOVERRIDES`, passed through as it stands.
///
/// The wine spelling exactly — `d3dx9_37=n;d3dx9_43=n`, modules comma-separated
/// on the left and `n`/`b` on the right — because there is no reason to invent a
/// second one for something an entry's author already knows how to write.
///
/// Unset, [`crate::newsys::windows`] fills it in from the DLLs a release ships
/// beside its executable: a demo that carries its own `d3dx9_37.dll` carries it
/// because it needs that one, and wine's builtin d3dx9 is not it.
pub const META_DLL_OVERRIDES: &str = "wine_dll_overrides";

/// Whether one is used when nothing says otherwise.
///
/// Off, because the desktop is a window manager of wine's own between the demo
/// and the screen: the picture goes through an extra composite, the demo's own
/// fullscreen becomes a window the size of the desktop, and anything the demo
/// does with the real display mode stops working. Most demos are happier
/// without it — but see [`META_DESKTOP`] for the ones that are not.
pub const DEFAULT_DESKTOP: bool = false;

/// Whether a compatibility profile is asked for when nothing says otherwise.
///
/// Off. It is Mesa-only (nothing else reads [`GL_COMPAT_OVERRIDE`]), it makes
/// every context on the demo's side a compatibility one, and the demos that need
/// it are the ones that resolve GL entry points without checking them — a
/// minority worth naming one at a time rather than a default worth carrying.
pub const DEFAULT_GL_COMPAT: bool = false;

/// What [`META_GL_COMPAT`] sets `MESA_GL_VERSION_OVERRIDE` to.
///
/// The `COMPAT` suffix is the operative half — it is what makes Mesa hand back a
/// compatibility context for a core request. `4.6` rather than the `3.3` the
/// demo asked for so that nothing else is taken away in the process: the version
/// is a ceiling, and lowering it to the request would be a second change nobody
/// asked for.
pub const GL_COMPAT_OVERRIDE: &str = "4.6COMPAT";

/// What a `pick` session runs at, since the size is not known until the person
/// watching has chosen one.
///
/// Big enough to hold anything a dialog of the era offers — 1600x1200 is the
/// tallest classic mode, 1920x1080 the widest — because whatever is picked has
/// to fit inside the session, and a mode taller than it comes out clipped (more
/// so with `wine_desktop`, where the desktop is a hard ceiling). The session is
/// scaled to the quad either way, so a demo that picks 640x480 gets a small
/// picture in the middle of it: the price of choosing late.
const PICK_RES: &str = "1920x1200";

/// The wine prefix demos are run in, under the user's home directory.
///
/// Deliberately not `~/.wine`: a demo is free to install fonts, codecs and DLL
/// overrides, and none of that belongs in the prefix the user runs their own
/// programs from. wine creates it on first use.
const PREFIX_DIR: &str = ".wine-demarc";

/// The dialog driver, relative to [`system_dir`].
const AUTODLG: &str = "win/demarc-autodlg.exe";

/// How long the driver keeps looking for a dialog before giving up. Generous: a
/// cold wine prefix spends a while building itself before the first window.
const DIALOG_TIMEOUT: f64 = 20.0;

/// Ticked on any demo that offers it: it saves a Windows title bar across the
/// top of the picture, and costs nothing — the session is already the size of
/// the mode, and under `wine_desktop` "fullscreen" means the size of that
/// desktop and nothing more.
const DEFAULT_CHECK: &str = "Fullscreen";

/// `WIDTHxHEIGHT`, or nothing.
fn parse_res(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The `WINEDLLOVERRIDES` an entry asks for, if it asks for one.
///
/// Whitespace-trimmed and nothing else: the value is wine's own syntax and goes
/// to wine unread — see [`META_DLL_OVERRIDES`]. An empty one is no override at
/// all rather than an empty variable, which to wine means "override nothing
/// with nothing" and is worth keeping out of the environment.
pub(crate) fn dll_overrides(meta: &HashMap<String, String>) -> Option<String> {
    meta.get(META_DLL_OVERRIDES)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Does an entry want a compatibility profile? See [`META_GL_COMPAT`].
pub(crate) fn gl_compat(meta: &HashMap<String, String>) -> bool {
    meta.get(META_GL_COMPAT)
        .map(|v| is_yes(v))
        .unwrap_or(DEFAULT_GL_COMPAT)
}

/// Is a meta value one of the ways of saying yes?
pub(crate) fn is_yes(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

pub(crate) fn has_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

pub(crate) fn wine_prefix() -> Result<PathBuf> {
    let home = dirs::home_dir().context("No home directory to put a wine prefix in")?;
    Ok(home.join(PREFIX_DIR))
}

/// What to do about the setup dialog nearly every PC demo opens with.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Dialog {
    /// Answer it: choose the `wine_res` mode and press Start.
    Drive,
    /// Leave it alone. `wine_res=pick` asks for this — for the demo whose
    /// dialog the driver reads wrongly, or the one with an option only a person
    /// can decide. Input reaches the session, so the dialog can be answered by
    /// hand exactly as it would be on Windows.
    ///
    /// The driver still runs (`--no-go`), because it is also what starts the
    /// demo and what reports its end; it just presses nothing and leaves the
    /// demo's window alone, title bar and all.
    Pick,
}

/// How a session is set up, resolved once from the entry's metadata.
#[derive(Clone)]
struct Config {
    exe: PathBuf,
    width: u32,
    height: u32,
    dialog: Dialog,
    /// Run inside `explorer /desktop=`, a wine virtual desktop the size of the
    /// session — see [`META_DESKTOP`].
    desktop: bool,
}

impl Config {
    fn from_meta(exe: &Path, meta: &HashMap<String, String>) -> Result<Self> {
        let res = meta
            .get(META_RES)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_RES);
        let dialog = if res.eq_ignore_ascii_case(PICK) {
            Dialog::Pick
        } else {
            Dialog::Drive
        };
        // A pick session's size is not a choice anyone made, so it is not the
        // one to warn about when it cannot be parsed.
        let wanted = if dialog == Dialog::Pick {
            PICK_RES
        } else {
            res
        };

        let (width, height) = parse_res(wanted).unwrap_or_else(|| {
            warn!("{META_RES}={res:?} is neither {PICK:?} nor a WIDTHxHEIGHT; using {DEFAULT_RES}");
            parse_res(DEFAULT_RES).expect("the default is a valid resolution")
        });
        Ok(Self {
            // wine takes a Unix path fine, but it has to be absolute: the demo
            // is started from its own directory, not from demarc's.
            exe: exe
                .canonicalize()
                .with_context(|| format!("No such executable: {}", exe.display()))?,
            width,
            height,
            dialog,
            desktop: meta
                .get(META_DESKTOP)
                .map(|v| is_yes(v))
                .unwrap_or(DEFAULT_DESKTOP),
        })
    }

    /// The arguments to `wine` that run this demo, dialog and all.
    ///
    /// No shell is involved, so nothing needs quoting — demo filenames are full
    /// of spaces, brackets and apostrophes.
    fn wine_args(&self, autodlg: Option<&Path>) -> Vec<String> {
        let mut args = Vec::new();
        if self.desktop {
            args.extend([
                "explorer".to_string(),
                format!("/desktop=demarc,{}x{}", self.width, self.height),
            ]);
        }
        // Only when there is no driver to run at all does the demo become the
        // command: without one nothing can report the demo's end, and the
        // session runs blind until it happens to exit.
        let Some(autodlg) = autodlg else {
            args.push(self.exe.to_string_lossy().into_owned());
            return args;
        };
        args.extend([
            autodlg.to_string_lossy().into_owned(),
            "--launch".into(),
            self.exe.to_string_lossy().into_owned(),
            "--timeout".into(),
            DIALOG_TIMEOUT.to_string(),
        ]);
        match self.dialog {
            Dialog::Drive => args.extend([
                "--prefer".into(),
                format!("{}x{}", self.width, self.height),
                "--check".into(),
                DEFAULT_CHECK.into(),
            ]),
            // Nothing pressed and nothing rearranged: the dialog is being
            // answered by a person, and the window they end up with is theirs
            // rather than a captured frame that has to start at the origin.
            Dialog::Pick => args.extend(["--no-go".into(), "--no-fill".into()]),
        }
        args
    }
}

/// The dialog driver, if it is where it should be.
///
/// Wanted whatever the dialog setting is: `pick` only turns off the pressing of
/// buttons, not the driver's other job of saying when the demo starts and ends.
///
/// Missing, a demo is still worth running — one whose dialog someone dismisses
/// by hand runs fine. What is lost with the driver is the end of the demo, so
/// the session will sit there until the next entry is asked for.
pub(crate) fn autodlg() -> Option<PathBuf> {
    let driver = system_dir().join(AUTODLG);
    if driver.is_file() {
        return Some(driver);
    }
    warn!(
        "No dialog driver at {driver:?} - the setup dialog will need answering, \
         and the demo's end will go unnoticed"
    );
    None
}

/// The wine command a Windows release runs under — see [`wine_command`].
pub(crate) struct WineCommand {
    /// `wine` and everything after it: the virtual desktop if one was asked
    /// for, the dialog driver if there is one, and the demo. Ready to be
    /// spawned as it stands — no shell is involved, so nothing is quoted and
    /// nothing may be re-split on spaces.
    pub argv: Vec<String>,
    /// The size the session has to be, which is not always the size an entry
    /// asked for: `wine_res=pick` has no size of its own and gets one big
    /// enough to hold whatever the dialog is asked for.
    pub width: u32,
    pub height: u32,
}

/// Work out how a release would be started, without starting it.
///
/// The gamescope core (`docs/GAMESCOPE.md`) is what runs it: left to itself it
/// would run `wine <exe>`, which is a demo sitting on its setup dialog with
/// nobody to answer it. What it is given instead is this — the dialog driver,
/// the resolution to pick, the virtual desktop if one was asked for — restated
/// as core options by [`crate::newsys::windows`].
pub(crate) fn wine_command(exe: &Path, meta: &HashMap<String, String>) -> Result<WineCommand> {
    let cfg = Config::from_meta(exe, meta)?;
    let mut argv = vec!["wine".to_string()];
    argv.extend(cfg.wine_args(autodlg().as_deref()));
    Ok(WineCommand {
        argv,
        width: cfg.width,
        height: cfg.height,
    })
}

/// Shut the wine prefix down: `wineserver -k` kills every process in it.
///
/// wine's service processes — `wineserver`, `services.exe`, `winedevice.exe`,
/// `explorer.exe`, `rpcss.exe` — put themselves in sessions of their own, so
/// killing the process tree a demo was started in does not reach them, and they
/// go on running for the life of the prefix. (Measured: `gamescope -- wine cmd
/// /c exit` against a cold prefix was still running twenty-five seconds later
/// with one `winedevice.exe` left under its reaper.)
///
/// Safe to do wholesale because the prefix is demarc's own — nothing of the
/// user's runs in `~/.wine-demarc`. What it rules out is two demos sharing that
/// prefix, since closing it for one closes it for both; a sandboxed session has
/// a prefix nobody else is in and never comes here at all, which is what lets
/// several run at once (see [`crate::wine_sandbox`]).
/// Waited for, but never for long: this runs on the way out of demarc, and a
/// wineserver that will not answer must not be able to hold the quit up.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub(crate) fn close_prefix(prefix: &Path) {
    if !has_tool("wineserver") {
        sweep_prefix(prefix, None);
        return;
    }
    let killed = Command::new("wineserver")
        .arg("-k")
        .env("WINEPREFIX", prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut killer) = killed else {
        debug!("Could not run wineserver -k on {prefix:?}: {killed:?}");
        sweep_prefix(prefix, None);
        return;
    };
    let deadline = Instant::now() + CLOSE_TIMEOUT;
    while Instant::now() < deadline {
        // A failure here is only ever "there was no server", which is the state
        // this wanted anyway.
        if matches!(killer.try_wait(), Ok(Some(_)) | Err(_)) {
            debug!("Closed the wine prefix {prefix:?}");
            sweep_prefix(prefix, None);
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    // Left to finish on its own rather than waited on. It is doing the right
    // work; nothing here depends on seeing it end — but it must not be swept
    // away while it is doing it.
    debug!("wineserver -k on {prefix:?} is taking its time; leaving it to it");
    sweep_prefix(prefix, Some(killer.id()));
}

/// Kill whatever is left in `prefix` that `wineserver -k` could not reach,
/// `except` one pid.
///
/// It cannot reach everything. A wine process outlives its own wineserver now
/// and then — a wedged `winedevice.exe` is the one this keeps meeting — and
/// once the server is gone there is nobody left to ask: `wineserver -k` finds
/// no server to talk to, says nothing, and the orphan stays for as long as the
/// machine is up. (Observed: thirty-seven of them left by earlier sessions,
/// and not one wineserver still running. Reproduced on demand by ending a demo
/// inside a session and closing the prefix afterwards.)
///
/// So they are matched the one way that still identifies them: the prefix in
/// their environment, compared whole. That is demarc's own prefix and nothing
/// of the user's runs in it, which is what makes killing on sight reasonable
/// here and would not anywhere else.
fn sweep_prefix(prefix: &Path, except: Option<u32>) {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    let me = std::process::id();
    let wanted = format!("WINEPREFIX={}", prefix.display());
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me || Some(pid) == except {
            continue;
        }
        // Unreadable is somebody else's process, and so not one of ours.
        let Ok(environ) = std::fs::read(entry.path().join("environ")) else {
            continue;
        };
        if !environ
            .split(|&b| b == 0)
            .any(|var| var == wanted.as_bytes())
        {
            continue;
        }
        debug!("Killing {pid}, left behind in {prefix:?}");
        // SAFETY: `kill` touches no memory of ours, and a pid read out of
        // `/proc` a moment ago is at worst gone by now.
        unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
}

#[cfg(test)]
#[path = "tests/wine_tests.rs"]
mod tests;
