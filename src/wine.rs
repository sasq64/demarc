use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, warn};

use crate::system_dir;

/// Meta key holding the size of the session, as `WIDTHxHEIGHT`.
pub const META_RES: &str = "wine_res";

pub const DEFAULT_RES: &str = "1920x1080";

/// Meta key holding the resolutions to ask the setup dialog for, best first,
/// comma separated. Unset means "whatever [`META_RES`] says".
pub const META_DIALOG_RES: &str = "wine_dialog_res";

/// The [`META_DIALOG_RES`] value that means "leave the dialog to me".
pub const PICK: &str = "pick";

/// What the screen is taken to be when `widescreen` — the key the frontend sets
/// from the window, see [`crate::newsys::META_WIDESCREEN`] — says nothing.
pub const DEFAULT_WIDESCREEN: bool = true;

/// The 16:9 modes, and the 4:3 ones worth trying whatever the screen is: plenty
/// of demos offer nothing else.
const WIDE_DIALOG_RES: &str = "1920x1080,1280x720,1024x576";
const NARROW_DIALOG_RES: &str = "1280x1024,1024x768,800x600";

/// What [`META_DIALOG_RES`] is when nothing says otherwise: the widescreen modes
/// first, and only on a screen shaped for them.
pub fn default_dialog_res(widescreen: bool) -> String {
    if widescreen {
        format!("{WIDE_DIALOG_RES},{NARROW_DIALOG_RES}")
    } else {
        NARROW_DIALOG_RES.to_string()
    }
}

/// Meta key asking for the demo to be run inside a wine virtual desktop.
pub const META_DESKTOP: &str = "wine_desktop";

/// Meta key asking Mesa for a compatibility profile whatever the demo requested.
pub const META_GL_COMPAT: &str = "wine_gl_compat";

/// Meta key holding wine's `WINEDLLOVERRIDES`, passed through as it stands.
pub const META_DLL_OVERRIDES: &str = "wine_dll_overrides";

/// Whether one is used when nothing says otherwise.
pub const DEFAULT_DESKTOP: bool = false;

/// Whether a compatibility profile is asked for when nothing says otherwise.
pub const DEFAULT_GL_COMPAT: bool = false;

/// What [`META_GL_COMPAT`] sets `MESA_GL_VERSION_OVERRIDE` to.
pub const GL_COMPAT_OVERRIDE: &str = "4.6COMPAT";

/// The wine prefix demos are run in, under the user's home directory.
const PREFIX_DIR: &str = ".wine-demarc";

/// The dialog driver, relative to [`system_dir`].
const AUTODLG: &str = "win/demarc-autodlg.exe";

/// How long the driver keeps looking for a dialog before giving up.
const DIALOG_TIMEOUT: f64 = 20.0;

const DEFAULT_CHECK: &str = "Fullscreen";

/// A comma separated list with the blanks taken out, as `--prefer` wants it.
fn clean_list(text: &str) -> String {
    text.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

/// `WIDTHxHEIGHT`, or nothing.
fn parse_res(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// The `WINEDLLOVERRIDES` an entry asks for, if it asks for one.
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

/// Where `name` is on `PATH`, if it is anywhere on it.
pub(crate) fn find_tool(name: &str) -> Option<PathBuf> {
    find_in(&std::env::var_os("PATH")?, name)
}

/// [`find_tool`] against a search path given rather than read, which is the
/// only way to have a test look at one it prepared.
fn find_in(search_path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(search_path)
        .map(|dir| dir.join(name))
        .find(|path| {
            path.metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
}

pub(crate) fn has_tool(name: &str) -> bool {
    find_tool(name).is_some()
}

pub(crate) fn wine_prefix() -> Result<PathBuf> {
    let home = dirs::home_dir().context("No home directory to put a wine prefix in")?;
    Ok(home.join(PREFIX_DIR))
}

/// One of the things a Windows release needs before it can run at all.
pub(crate) struct Need {
    /// What is being looked for, as the report names it.
    pub what: &'static str,
    /// Where it was found — or, when it was not, what to do about that.
    pub found: Result<PathBuf, String>,
}

/// What [`check_wine`] found: every requirement, in the order it looked.
pub(crate) struct WineCheck {
    pub needs: Vec<Need>,
}

impl WineCheck {
    /// Is everything there?
    pub fn ok(&self) -> bool {
        self.needs.iter().all(|need| need.found.is_ok())
    }

    /// The names of the missing pieces, for a one-line warning.
    pub fn missing(&self) -> String {
        self.needs
            .iter()
            .filter(|need| need.found.is_err())
            .map(|need| need.what)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// One line per requirement, for `--check-wine`.
    pub fn report(&self) -> String {
        let mut out = String::new();
        for need in &self.needs {
            let (mark, detail) = match &need.found {
                Ok(path) => ("ok     ", path.display().to_string()),
                Err(why) => ("MISSING", why.clone()),
            };
            out.push_str(&format!("  {:<7} {mark}  {detail}\n", need.what));
        }
        out.push_str(if self.ok() {
            "\nWindows releases can be run."
        } else {
            "\nWindows releases are disabled."
        });
        out
    }
}

/// Can a Windows release be run on this machine?
pub(crate) fn check_wine() -> WineCheck {
    let tool = |what: &'static str, fix: &str| Need {
        what,
        found: find_tool(what).ok_or_else(|| format!("not on PATH - {fix}")),
    };
    let prefix = match wine_prefix() {
        Ok(prefix) if prefix.is_dir() => Ok(prefix),
        Ok(prefix) => Err(format!(
            "{} is not there - create it with `just wine-prefix`",
            prefix.display()
        )),
        Err(err) => Err(err.to_string()),
    };
    WineCheck {
        needs: vec![
            tool("wine", "install wine"),
            tool("bwrap", "install bubblewrap"),
            Need {
                what: "prefix",
                found: prefix,
            },
        ],
    }
}

/// What to do about the setup dialog nearly every PC demo opens with.
#[derive(Clone, PartialEq, Debug)]
enum Dialog {
    /// Answer it: choose the first of these modes the dialog offers, and press
    /// Start.
    Drive(String),
    /// Leave it alone. `wine_dialog_res=pick` asks for this
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
        let said = |key: &str| {
            meta.get(key)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let res = said(META_RES).unwrap_or_else(|| DEFAULT_RES.to_string());
        let (width, height) = parse_res(&res).unwrap_or_else(|| {
            warn!("{META_RES}={res:?} is not a WIDTHxHEIGHT; using {DEFAULT_RES}");
            parse_res(DEFAULT_RES).expect("the default is a valid resolution")
        });

        // The dialog is asked for the session's own size unless an entry names
        // the modes to try itself — a demo whose dialog offers nothing like the
        // size we want still has to be given something it does offer.
        let dialog = match said(META_DIALOG_RES) {
            Some(list) if list.eq_ignore_ascii_case(PICK) => Dialog::Pick,
            list => Dialog::Drive(
                list.map(|list| clean_list(&list))
                    .filter(|modes| !modes.is_empty())
                    .unwrap_or_else(|| format!("{width}x{height}")),
            ),
        };
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
        match &self.dialog {
            Dialog::Drive(modes) => args.extend([
                "--prefer".into(),
                modes.clone(),
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
    /// The size the session has to be.
    pub width: u32,
    pub height: u32,
}

/// Work out how a release would be started, without starting it.
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
