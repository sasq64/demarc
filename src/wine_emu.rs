//! Windows demos: a PE executable run under [wine] inside a fullscreen
//! [gamescope], drawn straight to the screen on top of demarc.
//!
//! Every other backend here is a picture source: demarc steps it, takes its
//! frame, and puts that frame on a quad. This one is not. wine renders through
//! the real GPU into a gamescope session of its own that covers the screen, and
//! demarc keeps running underneath it with a black frame and no audio — the
//! demo's own window *is* the display, and its sound goes to the user's speakers
//! by the usual route.
//!
//! That is the whole point of doing it this way. Capturing wine's output back
//! into demarc is possible — a headless gamescope publishing a PipeWire node,
//! `gst-launch` on one pipe and `pw-record` on another — but it costs a
//! composite, a copy and a resample per frame for a picture that then gets
//! uploaded and drawn again, and it puts three more processes in the way of a
//! demo that was going to run at 60fps on its own. On top is cheap and looks
//! right; the price is that shaders, the grid and screenshots don't apply to it.
//!
//! What runs, then, is one command:
//!
//! ```text
//! WINEPREFIX=~/.wine-demos gamescope -w 800 -h 600 -f -- \
//!     wine demarc-autodlg.exe --launch demo.exe --prefer 800x600
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
//!   dialogs is only half of what it does. The other half is telling demarc
//!   what the demo is doing, which nothing out here can see: gamescope waits on
//!   its whole process tree, and wine's services outlive the demo inside it, so
//!   the process demarc started stays alive long after the picture has gone.
//!   The driver holds the demo's own handle, and writes a line when it starts
//!   and another when it ends — see [`Signals`].
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
//! [gamescope]: https://github.com/ValveSoftware/gamescope

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use bevy::prelude::*;
use bevy::window::{Monitor, PrimaryWindow, WindowMode};
use tracing::{debug, info, warn};

use crate::retro_emu::Backend;
use crate::screensaver::covers_a_monitor;
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

/// Prefix of the lines the driver writes for demarc rather than for the log.
/// Its other half is `SENTINEL` in `tools/autodlg`.
const SENTINEL: &str = "!demarc ";

/// What [`drain`] calls the stream the driver writes on, which is wine's
/// stdout. gamescope's own noise comes up the other one.
const DRIVER_STREAM: &str = "autodlg";

/// Meta key asking for the demo to be run inside a wine virtual desktop.
pub const META_DESKTOP: &str = "wine_desktop";

/// Whether one is used when nothing says otherwise.
///
/// Off, because the desktop is a window manager of wine's own between the demo
/// and the screen: the picture goes through an extra composite, the demo's own
/// fullscreen becomes a window the size of the desktop, and anything the demo
/// does with the real display mode stops working. Most demos are happier
/// without it — but see [`META_DESKTOP`] for the ones that are not.
pub const DEFAULT_DESKTOP: bool = false;

/// What a `pick` session runs at, since the size is not known until the person
/// watching has chosen one.
///
/// Big enough to hold anything a dialog of the era offers — 1600x1200 is the
/// tallest classic mode, 1920x1080 the widest — because whatever is picked has
/// to fit inside the session, and a mode taller than it comes out clipped (more
/// so with `wine_desktop`, where the desktop is a hard ceiling). gamescope
/// scales the result to the screen either way, so a demo that picks 640x480
/// gets a small picture in the middle of it: the price of choosing late.
const PICK_RES: &str = "1920x1200";

/// The wine prefix demos are run in, under the user's home directory.
///
/// Deliberately not `~/.wine`: a demo is free to install fonts, codecs and DLL
/// overrides, and none of that belongs in the prefix the user runs their own
/// programs from. wine creates it on first use.
const PREFIX_DIR: &str = ".wine-demos";

/// The dialog driver, relative to [`system_dir`].
const AUTODLG: &str = "win/demarc-autodlg.exe";

/// How long the driver keeps looking for a dialog before giving up. Generous: a
/// cold wine prefix spends a while building itself before the first window.
const DIALOG_TIMEOUT: f64 = 20.0;

/// Ticked on any demo that offers it: it saves a Windows title bar across the
/// top of the picture, and costs nothing — gamescope is already the size of the
/// mode, and under `wine_desktop` "fullscreen" means the size of that desktop
/// and nothing more.
const DEFAULT_CHECK: &str = "Fullscreen";

/// How long a session waits to hear that the demo has started before giving up
/// on it.
///
/// Generous, since it covers gamescope coming up, wine building a cold prefix,
/// and the driver loading, all before the demo is so much as created. What it
/// catches is the session where none of that works: without it a demo that
/// never starts leaves a black gamescope over demarc for good, because the
/// thing demarc is watching is alive and well and simply has nothing in it.
const START_TIMEOUT: Duration = Duration::from_secs(60);

/// Programs a session needs, named here so a missing one is one clear error at
/// load time rather than a confusing failure later.
const REQUIRED_TOOLS: [&str; 2] = ["gamescope", "wine"];

/// Pace demarc's own (black) frames at something ordinary. Nothing depends on
/// it — there is no picture to keep in step with — but the frontend divides by
/// it, so it has to be a real number.
const FPS: f64 = 60.0;

/// Claimed audio rate. No samples are ever handed over; wine plays to the
/// user's sound card directly.
const SAMPLE_RATE: f64 = 44100.0;

/// `WIDTHxHEIGHT`, or nothing.
fn parse_res(text: &str) -> Option<(u32, u32)> {
    let (w, h) = text.trim().split_once(['x', 'X'])?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// Is a meta value one of the ways of saying yes?
fn is_yes(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

fn has_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

fn wine_prefix() -> Result<PathBuf> {
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
    /// can decide. gamescope has the keyboard and mouse, so the dialog can be
    /// answered by hand exactly as it would be on Windows.
    ///
    /// The driver still runs (`--no-go`), because it is also what starts the
    /// demo and what reports its end; it just presses nothing and leaves the
    /// demo's window alone, title bar and all.
    Pick,
}

/// How a session is set up, resolved once from the entry's metadata so
/// [`Backend::reset`] can build an identical one.
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

    fn frame_len(&self) -> usize {
        self.width as usize * self.height as usize
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
        // session runs blind until gamescope happens to exit.
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

/// How many demo sessions are on screen right now — one at most, but a count
/// rather than a flag so that a session starting before the last one has been
/// dropped cannot leave it stuck.
///
/// Read by [`restore_window`], which is the whole reason it exists; see
/// [`WinePlugin`] for what the frontend does about it.
static SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Is a demo holding the screen?
fn demo_on_screen() -> bool {
    let count = SESSIONS.load(Ordering::Relaxed);
    count > 0
}

/// What the dialog driver has said about the demo, as [`drain`] reads it off
/// the driver's stdout.
///
/// This is the only honest account of the demo there is. gamescope's exit is
/// not one: it launches its command under a `gamescopereaper` that waits for
/// the whole process tree, and wine's services — `wineserver`, `services.exe`,
/// `winedevice.exe` — put themselves in sessions of their own and go on running
/// for the life of the prefix, so the reaper waits on them long after the demo
/// has gone. (Measured: `gamescope -- wine cmd /c exit` against a cold prefix
/// was still running twenty-five seconds later with one `winedevice.exe` left
/// under its reaper.)
#[derive(Default)]
struct Signals {
    /// The demo process was created. Its absence is what [`START_TIMEOUT`]
    /// measures.
    started: AtomicBool,
    /// The demo is over: it exited, or it never started at all.
    ended: AtomicBool,
    /// Anything at all arrived on the driver's stream.
    ///
    /// The guard on [`START_TIMEOUT`], and the reason it can be trusted to end
    /// a session. A driver too old to report anything still says what it is
    /// doing (`launched ... as pid ...`), and so does a demo that writes to its
    /// stdout — either way something in there is alive, whatever it does or
    /// does not tell us, and the timeout keeps its hands off it. A session with
    /// nothing to show after a minute of silence is the one it is for.
    heard: AtomicBool,
}

/// One running demo: the gamescope process tree and the thread draining its
/// output. Dropping it takes the whole tree down.
struct Session {
    gamescope: Child,
    /// Set to stop the logger. Without it a quit can hang forever — see
    /// [`drain`] and [`Session::drop`].
    stop: Arc<AtomicBool>,
    logger: Option<JoinHandle<()>>,
    /// The prefix this demo runs in, so [`Drop`] can tell wine to close it.
    prefix: PathBuf,
    /// What the driver has told us, filled in by the logger thread.
    signals: Arc<Signals>,
    /// Whether there is a driver in the command to hear it from. Without one
    /// the only news of the demo is gamescope exiting, which may never come.
    driven: bool,
    /// When the session was started, for [`START_TIMEOUT`].
    began: Instant,
    /// Whether [`Session::exited`] has already reaped the child. A reaped pid
    /// belongs to the kernel again and may have been handed to somebody else,
    /// so it must never be signalled after this.
    reaped: bool,
    /// Whether this session is still counted in [`SESSIONS`]. Cleared the
    /// moment gamescope is gone rather than when the backend is dropped: the
    /// screen comes back to demarc when the window closes, and the frontend
    /// hangs on to a finished backend until its idle timeout moves it along.
    on_screen: bool,
}

impl Session {
    fn start(cfg: &Config) -> Result<Self> {
        if let Some(missing) = REQUIRED_TOOLS.iter().find(|t| !has_tool(t)) {
            bail!(
                "Running Windows demos needs `{missing}`, which is not on PATH \
                 (this backend runs wine inside gamescope)"
            );
        }
        // Wanted whatever the dialog setting is: `pick` only turns off the
        // pressing of buttons, not the driver's other job of saying when the
        // demo starts and ends.
        let driver = system_dir().join(AUTODLG);
        let autodlg = if driver.is_file() {
            Some(driver)
        } else {
            // Worth running anyway: a demo whose dialog someone dismisses by
            // hand still runs. What is lost with the driver is the end of the
            // demo — see [`Signals`] — so the session will sit there until the
            // next entry is asked for.
            warn!(
                "No dialog driver at {driver:?} - the setup dialog will need answering, \
                 and the demo's end will go unnoticed"
            );
            None
        };

        let prefix = wine_prefix()?;
        // Anything still running in the prefix is left over from a session that
        // never got to `Drop` — a demarc that was killed, or crashed. Clearing
        // it here keeps those from piling up one set per launch, and costs
        // nothing when there is nothing to clear.
        close_prefix(&prefix);
        let wine_args = cfg.wine_args(autodlg.as_deref());
        debug!("wine {}", wine_args.join(" "));

        let mut command = Command::new("gamescope");
        command
            .args([
                "-w",
                &cfg.width.to_string(),
                "-h",
                &cfg.height.to_string(),
                "-f",
                "--",
                "wine",
            ])
            .args(&wine_args)
            // A release that ships a `data/` folder or its own `fmod.dll` finds
            // neither when started from wherever demarc was launched, and fails
            // silently - a dialog that works and then a black screen.
            .current_dir(cfg.exe.parent().unwrap_or(Path::new(".")))
            .env("WINEPREFIX", &prefix)
            .env(
                "WINEDEBUG",
                std::env::var("WINEDEBUG").unwrap_or_else(|_| "-all".into()),
            )
            .stdin(Stdio::null())
            // Both pipes are drained on a thread below. They have to be: wine
            // and gamescope are chatty enough to fill a pipe and block on it.
            // What the dialog driver prints comes this way too when wine passes
            // its console on, which is the only window there is onto what
            // happened inside the virtual desktop.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        session_leader(&mut command);

        let mut gamescope = command.spawn().context("Failed to start gamescope")?;
        let streams = [
            gamescope
                .stdout
                .take()
                .map(|out| (OwnedFd::from(out), DRIVER_STREAM)),
            gamescope
                .stderr
                .take()
                .map(|err| (OwnedFd::from(err), "gamescope")),
        ];
        let stop = Arc::new(AtomicBool::new(false));
        let signals = Arc::new(Signals::default());
        let logger = {
            let stop = Arc::clone(&stop);
            let signals = Arc::clone(&signals);
            thread::Builder::new()
                .name("wine-emu-log".into())
                .spawn(move || drain(streams.into_iter().flatten().collect(), &stop, &signals))?
        };

        info!(
            "Running {:?} under wine in {}x{} gamescope ({}), prefix {:?}",
            cfg.exe,
            cfg.width,
            cfg.height,
            match cfg.dialog {
                Dialog::Drive => "driving the setup dialog",
                Dialog::Pick => "setup dialog left to you",
            },
            prefix
        );
        SESSIONS.fetch_add(1, Ordering::Relaxed);
        Ok(Session {
            gamescope,
            stop,
            logger: Some(logger),
            prefix,
            signals,
            driven: autodlg.is_some(),
            began: Instant::now(),
            reaped: false,
            on_screen: true,
        })
    }

    /// Is the demo over?
    ///
    /// Three ways to tell, and the order matters. The driver's word comes
    /// first, because it is the only one that is actually about the demo. Then
    /// a driver that never reported the demo starting at all, which is a
    /// session where something went wrong before there was anything to watch.
    /// Last, the process going away — true when it happens, but for a demo
    /// under wine it usually does not: see [`Signals`].
    fn finished(&mut self) -> bool {
        if self.signals.ended.load(Ordering::Relaxed) {
            self.left_the_screen();
            return true;
        }
        if self.driven
            && !self.signals.started.load(Ordering::Relaxed)
            && !self.signals.heard.load(Ordering::Relaxed)
            && self.began.elapsed() > START_TIMEOUT
        {
            warn!("Nothing started inside the session after {START_TIMEOUT:?}; giving up on it");
            self.left_the_screen();
            return true;
        }
        self.exited()
    }

    /// Has the gamescope process gone?
    ///
    /// The backstop under [`Session::finished`], not the test itself: under
    /// wine this usually stays false long after the demo has ended (see
    /// [`Signals`]), but it is the only answer there is when there is no driver
    /// to ask, and it is the truth when it does happen.
    ///
    /// Reaps the child when it has gone, and remembers that: past this point
    /// the pid is no longer ours to signal.
    fn exited(&mut self) -> bool {
        if self.reaped {
            return true;
        }
        self.reaped = matches!(self.gamescope.try_wait(), Ok(Some(_)) | Err(_));
        if self.reaped {
            self.left_the_screen();
        }
        self.reaped
    }

    /// Stop counting this session as being on screen. Idempotent, since both
    /// the demo ending and the backend being dropped get here.
    fn left_the_screen(&mut self) {
        if std::mem::take(&mut self.on_screen) {
            SESSIONS.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // First, so the logger is on its way out whatever the rest of this does.
        self.stop.store(true, Ordering::Relaxed);
        self.left_the_screen();

        if !self.reaped {
            // Negative pid: the whole process group, which thanks to
            // `session_leader` is exactly this gamescope and everything it
            // started. Signalling gamescope alone leaves a `gamescopereaper`
            // and the entire wine process tree — demo included — running over
            // the desktop. Skipped once the child has been reaped, since the
            // kernel is free to hand that pid to somebody else afterwards.
            //
            // SAFETY: `kill` touches no memory of ours, and the group is one we
            // made.
            unsafe { libc::kill(-(self.gamescope.id() as i32), libc::SIGKILL) };
            let _ = self.gamescope.wait();
        }
        close_prefix(&self.prefix);

        // Bounded, because `drain` polls rather than blocks: the pipes can
        // still be open here (see `close_prefix`), and joining a thread parked
        // in `read` on one of them is a hang with no way out.
        if let Some(logger) = self.logger.take() {
            let _ = logger.join();
        }
    }
}

/// Shut the wine prefix down: `wineserver -k` kills every process in it.
///
/// The group kill above does not reach these. wine's service processes —
/// `wineserver`, `services.exe`, `winedevice.exe`, `explorer.exe`, `rpcss.exe`
/// — put themselves in sessions of their own, so they survive it, and they
/// inherited demarc's stdout and stderr on the way, which is what used to wedge
/// the quit: those pipes never reach EOF while a service still holds one.
/// (Observed: a quit after one demo left thirteen wine processes holding the
/// pipe, and demarc parked in `join` for as long as it was left alone.)
///
/// Safe to do wholesale because the prefix is this backend's own — nothing of
/// the user's runs in `~/.wine-demos`. The one thing it rules out is two
/// Windows demos at once, which a backend that takes the whole screen could not
/// do anyway.
/// Waited for, but never for long: this runs on the way out of demarc, and a
/// wineserver that will not answer must not be able to hold the quit up.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

fn close_prefix(prefix: &Path) {
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
/// inside gamescope and closing the prefix afterwards.)
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

/// Read `streams` into the log until they end or `stop` is set.
///
/// Draining them is not optional — gamescope and wine are chatty enough to fill
/// a pipe and then block on it, taking the demo with them — but neither is
/// being able to stop: the write ends outlive the process tree we started (see
/// [`close_prefix`]), so a plain blocking read is a thread that may never come
/// back. Hence one thread polling both non-blocking fds, checking `stop`
/// between polls, and logging whole lines as they arrive.
fn drain(streams: Vec<(OwnedFd, &'static str)>, stop: &AtomicBool, signals: &Signals) {
    /// How long a poll waits, and so the longest a quit waits for this thread.
    const POLL_MS: libc::c_int = 100;
    /// Log a partial line rather than buffer without end, for a source that
    /// writes a progress bar or a prompt and no newline.
    const MAX_LINE: usize = 8 * 1024;

    let mut open: Vec<(File, &'static str, Vec<u8>)> = streams
        .into_iter()
        .map(|(fd, name)| {
            // SAFETY: the fd is ours, still open, and nothing else touches it.
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) };
            (File::from(fd), name, Vec::new())
        })
        .collect();

    let mut buf = [0u8; 4096];
    while !open.is_empty() && !stop.load(Ordering::Relaxed) {
        let mut fds: Vec<libc::pollfd> = open
            .iter()
            .map(|(file, _, _)| libc::pollfd {
                fd: file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is a live slice of the right length; poll writes only
        // into its `revents`.
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_MS) } < 0 {
            return;
        }

        let mut ended = Vec::new();
        for (i, (file, name, line)) in open.iter_mut().enumerate() {
            if fds[i].revents == 0 {
                continue;
            }
            match file.read(&mut buf) {
                // End of stream: everything that had it open has let go.
                Ok(0) => ended.push(i),
                Ok(n) => {
                    line.extend_from_slice(&buf[..n]);
                    while let Some(at) = line.iter().position(|&b| b == b'\n') {
                        let rest = line.split_off(at + 1);
                        read_line(name, &line[..at], signals);
                        *line = rest;
                    }
                    if line.len() >= MAX_LINE {
                        read_line(name, line, signals);
                        line.clear();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => ended.push(i),
            }
        }
        for i in ended.into_iter().rev() {
            let (_, name, line) = open.remove(i);
            if !line.is_empty() {
                read_line(name, &line, signals);
            }
        }
    }
}

/// Log one line — unless it is one of the driver's, in which case it is news
/// about the demo rather than something to read. See [`Signals`].
fn read_line(name: &str, line: &[u8], signals: &Signals) {
    let text = String::from_utf8_lossy(line);
    let text = text.trim_end_matches('\r');
    // Only the driver's own stream counts: gamescope talks on the other one
    // whether or not anything ever runs inside it.
    if name == DRIVER_STREAM {
        signals.heard.store(true, Ordering::Relaxed);
    }
    let Some(event) = text.trim_start().strip_prefix(SENTINEL) else {
        debug!("{name}: {text}");
        return;
    };
    match event.trim() {
        "started" => {
            debug!("The demo is running");
            signals.started.store(true, Ordering::Relaxed);
        }
        "exited" => {
            debug!("The driver says the demo has ended");
            signals.ended.store(true, Ordering::Relaxed);
        }
        "failed" => {
            warn!("The driver could not start the demo");
            signals.ended.store(true, Ordering::Relaxed);
        }
        // A driver newer than this demarc. Nothing to do, but worth seeing.
        other => debug!("Unknown report from the driver: {other:?}"),
    }
}

/// Put the child in a session (and so a process group) of its own, and tie its
/// life to ours.
///
/// The session is what lets [`Drop`] signal the whole tree, and it keeps a demo
/// that grabs the keyboard from pulling demarc's terminal signals along with it.
/// The death signal is the other half of the same worry: a demarc that dies
/// without running `Drop` — a panic, a `kill -9`, a crash — would otherwise leave
/// a fullscreen gamescope and a wine process tree sitting over the desktop with
/// nothing left to close them.
fn session_leader(cmd: &mut Command) {
    // SAFETY: `pre_exec` runs between fork and exec, where only
    // async-signal-safe calls are allowed. Both of these are bare syscalls.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

/// Puts demarc's window back the way a demo found it.
///
/// A session is a second fullscreen window arriving on the compositor's
/// workspace, and a good number of them — Hyprland among them — hand fullscreen
/// to the newcomer instead of stacking, which drops demarc to a floating window
/// underneath for as long as the demo runs and leaves it there afterwards.
///
/// Stopping that is not demarc's to do: the demo is *meant* to be on top, and a
/// demarc that argued for fullscreen while one ran would be arguing to cover
/// the picture the user is watching. (Hyprland can be told to hand the state
/// over and give it back instead — `misc:new_window_takes_over_fullscreen = 1`
/// — but that is the user's config, not ours.) So the state is noted when the
/// demo takes the screen and asked for again once it has given it back.
pub struct WinePlugin;

impl Plugin for WinePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Restore>()
            .add_systems(Update, restore_window);
    }
}

/// How far along putting the window back we are.
#[derive(Resource, Default)]
struct Restore {
    /// Whether a demo held the screen when this last ran.
    demo_up: bool,
    /// Whether demarc was covering a monitor when the demo took it. Nothing to
    /// put back if it wasn't — `--window`, or a demo started from a windowed
    /// demarc.
    was_fullscreen: bool,
    /// Set for the one frame between letting fullscreen go and asking for it
    /// again — see [`restore_window`].
    toggling: bool,
}

fn restore_window(
    mut state: ResMut<Restore>,
    window: Single<&mut Window, With<PrimaryWindow>>,
    monitors: Query<&Monitor>,
) {
    let mut window = window.into_inner();

    // `Window::mode` is a request that winit only acts on when it changes, and
    // a compositor that took fullscreen away did so without touching it — so
    // the mode we want may well be the mode already written there. Hence the
    // two halves: let it go on the frame the demo ends, ask for it on the next.
    if state.toggling {
        state.toggling = false;
        debug!("Actually setting fullscreen");
        window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Current);
        return;
    }

    let demo_up = demo_on_screen();
    if demo_up == state.demo_up {
        return;
    }
    state.demo_up = demo_up;

    if demo_up {
        // Read before gamescope has had time to map its window, which is what
        // makes this the state to go back to.
        state.was_fullscreen = covers_a_monitor(&window, &monitors);
        debug!(
            "A demo has the screen (demarc was fullscreen: {}) {}x{}",
            state.was_fullscreen,
            window.width(),
            window.height()
        );
        return;
    }
    let covers = covers_a_monitor(&window, &monitors);
    debug!(
        "Demo ended. covered: {covers} {}x{}",
        window.width(),
        window.height()
    );
    // Nothing to do for a compositor that gave the fullscreen back itself, and
    // nothing to do if there was none to give back.
    if !state.was_fullscreen || covers {
        return;
    }
    debug!("The demo left demarc windowed; going back to fullscreen");
    window.mode = WindowMode::Windowed;
    state.toggling = true;
}

/// A Windows demo running on top of demarc. See the module docs.
pub struct WineEmu {
    cfg: Config,
    session: Option<Session>,
    /// One black frame at the session size, handed over unchanged for as long
    /// as the demo runs.
    frame: Vec<u32>,
    /// [`Backend::frame_hash`], which never moves because the frame never
    /// changes. It is non-zero only so that it differs from the frontend's own
    /// starting value: that one upload is what clears the last demo's picture
    /// off the quad, and after it there is nothing more to send.
    serial: u64,
    /// Set when the demo has gone. The frontend's idle timeout then moves on to
    /// the next entry, the same as it would for a core sitting on a dead screen.
    finished: bool,
}

impl WineEmu {
    pub fn new(exe: &Path, meta: HashMap<String, String>) -> Result<Self> {
        let cfg = Config::from_meta(exe, &meta)?;
        let session = Session::start(&cfg)?;
        Ok(Self {
            frame: vec![u32::from_ne_bytes([0, 0, 0, 255]); cfg.frame_len()],
            cfg,
            session: Some(session),
            serial: 1,
            finished: false,
        })
    }
}

impl Backend for WineEmu {
    /// Nothing to step: the demo is running on the GPU on its own clock. All
    /// this does is notice when it has stopped.
    fn run(&mut self) -> bool {
        if !self.finished
            && let Some(session) = self.session.as_mut()
            && session.finished()
        {
            info!("The Windows demo has exited");
            // Taken down here and now, rather than left for the frontend to
            // drop when its idle timeout moves the entry along. gamescope
            // outlives the demo (see [`Signals`]), so what is left is a
            // fullscreen session with nothing in it sitting over demarc — and
            // the wine services it is waiting on, which `Drop` is what closes.
            self.session = None;
            self.finished = true;
        }
        true
    }

    fn frame_hash(&self) -> u64 {
        self.serial
    }

    /// True once the demo is gone, and never before: an idle backend is one the
    /// frontend is free to skip past, and a running demo is anything but — its
    /// picture just isn't ours.
    fn is_idle(&self) -> bool {
        self.finished
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(
            self.cfg.width as usize,
            self.cfg.height as usize,
            &self.frame,
        );
    }

    /// No samples, ever: wine plays to the user's sound card itself.
    fn with_audio(&mut self, _f: &mut dyn FnMut(&[i16])) {}

    fn get_frame_size(&self) -> (usize, usize) {
        (self.cfg.width as usize, self.cfg.height as usize)
    }

    fn aspect_ratio(&self) -> f32 {
        if self.cfg.height == 0 {
            0.0
        } else {
            self.cfg.width as f32 / self.cfg.height as f32
        }
    }

    fn sample_rate(&self) -> f64 {
        SAMPLE_RATE
    }

    fn fps(&self) -> f64 {
        FPS
    }

    fn reset(&mut self) {
        // Dropped first: the new session cannot have the screen while the old
        // one still holds it.
        self.session = None;
        match Session::start(&self.cfg) {
            Ok(session) => {
                self.session = Some(session);
                self.finished = false;
            }
            Err(err) => {
                warn!("Could not restart the demo: {err}");
                self.finished = true;
            }
        }
    }

    // Input goes to the demo, which has the screen and the keyboard; none of it
    // arrives here.
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}

    // No disks, and nothing to fast-forward through.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        0
    }
    fn skip_frames(&mut self, _frames: u32) {}

    fn get_info(&self) -> Option<String> {
        let mode = match self.cfg.dialog {
            Dialog::Drive => "",
            Dialog::Pick => ", pick your own settings",
        };
        let desktop = if self.cfg.desktop {
            ", virtual desktop"
        } else {
            ""
        };
        Some(format!(
            "Windows demo {}x{} (wine + gamescope{desktop}{mode})",
            self.cfg.width, self.cfg.height
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::FromRawFd;

    /// A pipe, as `(read end, write end)`.
    fn pipe() -> (File, File) {
        let mut fds = [0; 2];
        // SAFETY: `pipe` writes two fds into an array of the right size.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "no pipe");
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    fn joins_within(handle: JoinHandle<()>, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while !handle.is_finished() {
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
        handle.join().is_ok()
    }

    fn drain_thread(read: File, stop: &Arc<AtomicBool>, signals: &Arc<Signals>) -> JoinHandle<()> {
        let stop = Arc::clone(stop);
        let signals = Arc::clone(signals);
        // Labelled as the driver's stream: that is the one the reports and the
        // signs of life come up.
        thread::spawn(move || drain(vec![(OwnedFd::from(read), DRIVER_STREAM)], &stop, &signals))
    }

    /// The reader has to keep up — gamescope and wine fill a pipe and then
    /// block on it, taking the demo down with them — and it has to notice the
    /// end when it comes.
    #[test]
    fn drains_a_pipe_and_returns_when_it_ends() {
        let (read, mut write) = pipe();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = drain_thread(read, &stop, &Arc::new(Signals::default()));

        // Several times a pipe buffer, so these writes can only complete
        // because something is emptying the other end.
        let filler = "x".repeat(1000);
        for i in 0..256 {
            writeln!(write, "line {i} {filler}").expect("the reader stopped reading");
        }
        // ...and a last line with no newline on it, which still gets logged.
        write!(write, "no newline here").unwrap();
        drop(write);

        assert!(
            joins_within(reader, Duration::from_secs(5)),
            "the reader did not notice the pipe ending"
        );
    }

    /// The bug this backend shipped with: wine's service processes put
    /// themselves in their own sessions and inherit demarc's stdout and stderr,
    /// so the pipes stay open after the demo's whole process group is killed.
    /// A reader parked in `read` on one of those could not be joined, and the
    /// quit hung in `Session::drop` for as long as it was left alone.
    #[test]
    fn a_pipe_someone_else_holds_open_cannot_wedge_the_quit() {
        // Kept alive to the end of the test, the way `winedevice.exe` keeps its
        // copy of the write end.
        let (read, _write) = pipe();
        let stop = Arc::new(AtomicBool::new(false));
        let reader = drain_thread(read, &stop, &Arc::new(Signals::default()));

        thread::sleep(Duration::from_millis(300));
        assert!(
            !reader.is_finished(),
            "the reader gave up on a pipe that is still open"
        );

        stop.store(true, Ordering::Relaxed);
        assert!(
            joins_within(reader, Duration::from_secs(2)),
            "the reader would not stop - a quit here hangs demarc"
        );
    }

    /// Wait for `f`, or give up. The reports arrive on the logger thread.
    fn becomes_true(mut f: impl FnMut() -> bool, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while !f() {
            if Instant::now() > deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// The driver's reports are the only honest account of the demo there is —
    /// gamescope outlives it, see `Signals` — so they have to survive the trip
    /// through the log pipe: mixed in with everything wine has to say, and read
    /// as news rather than logged and forgotten.
    #[test]
    fn the_drivers_reports_come_back_through_the_log() {
        let (read, mut write) = pipe();
        let stop = Arc::new(AtomicBool::new(false));
        let signals = Arc::new(Signals::default());
        let reader = drain_thread(read, &stop, &signals);

        writeln!(write, "fixme:win:something wine has to say").unwrap();
        // The driver is a PE writing to a pipe, so its lines arrive with the
        // carriage return still on them.
        write!(write, "{SENTINEL}started\r\n").unwrap();
        assert!(
            becomes_true(
                || signals.started.load(Ordering::Relaxed),
                Duration::from_secs(2)
            ),
            "the start of the demo was not heard"
        );
        assert!(
            !signals.ended.load(Ordering::Relaxed),
            "a running demo was taken for a finished one"
        );

        writeln!(write, "undecorated the demo window (800x600)").unwrap();
        writeln!(write, "{SENTINEL}exited").unwrap();
        assert!(
            becomes_true(
                || signals.ended.load(Ordering::Relaxed),
                Duration::from_secs(2)
            ),
            "the end of the demo was not heard - the session would hold the screen"
        );

        drop(write);
        assert!(joins_within(reader, Duration::from_secs(5)));
    }

    /// A demo that never starts has to end the session too, or demarc sits
    /// behind an empty gamescope forever.
    #[test]
    fn a_demo_that_never_started_ends_the_session() {
        let (read, mut write) = pipe();
        let stop = Arc::new(AtomicBool::new(false));
        let signals = Arc::new(Signals::default());
        let reader = drain_thread(read, &stop, &signals);

        writeln!(write, "{SENTINEL}failed").unwrap();
        assert!(
            becomes_true(
                || signals.ended.load(Ordering::Relaxed),
                Duration::from_secs(2)
            ),
            "a demo that could not be started was left running"
        );
        assert!(!signals.started.load(Ordering::Relaxed));

        drop(write);
        assert!(joins_within(reader, Duration::from_secs(5)));
    }

    /// A driver too old to report anything must not have its demo shot at
    /// `START_TIMEOUT`: it still says what it is doing, and that is enough to
    /// know something is running in there.
    #[test]
    fn an_older_drivers_chatter_still_counts_as_a_sign_of_life() {
        let (read, mut write) = pipe();
        let stop = Arc::new(AtomicBool::new(false));
        let signals = Arc::new(Signals::default());
        let reader = drain_thread(read, &stop, &signals);

        writeln!(write, "launched /demos/x.exe as pid 42").unwrap();
        assert!(
            becomes_true(
                || signals.heard.load(Ordering::Relaxed),
                Duration::from_secs(2)
            ),
            "a driver that is plainly running was not heard"
        );
        assert!(!signals.started.load(Ordering::Relaxed));
        assert!(!signals.ended.load(Ordering::Relaxed));

        drop(write);
        assert!(joins_within(reader, Duration::from_secs(5)));
    }

    /// The pile-up this exists to stop: a process left in the prefix that
    /// `wineserver -k` cannot reach, because the server it belonged to has
    /// already gone. Stood in for here by a `sleep` carrying the same
    /// `WINEPREFIX`, which is all the sweep matches on.
    #[test]
    fn a_process_left_in_the_prefix_is_swept_up() {
        let prefix = std::env::temp_dir().join(format!("demarc-sweep-{}", std::process::id()));
        let mut leftover = Command::new("sleep")
            .arg("60")
            .env("WINEPREFIX", &prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");
        // A neighbour in a prefix of its own, to be left alone.
        let mut other = Command::new("sleep")
            .arg("60")
            .env("WINEPREFIX", prefix.join("elsewhere"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");

        sweep_prefix(&prefix, None);
        assert!(
            becomes_true(
                || matches!(leftover.try_wait(), Ok(Some(_))),
                Duration::from_secs(5)
            ),
            "the leftover survived the sweep"
        );
        assert!(
            matches!(other.try_wait(), Ok(None)),
            "the sweep took something out of another prefix"
        );

        let _ = other.kill();
        let _ = other.wait();
    }

    /// And the pid it is told to spare — the `wineserver -k` still working
    /// through the prefix — has to survive it.
    #[test]
    fn the_sweep_spares_the_pid_it_is_given() {
        let prefix = std::env::temp_dir().join(format!("demarc-spare-{}", std::process::id()));
        let mut spared = Command::new("sleep")
            .arg("60")
            .env("WINEPREFIX", &prefix)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("sleep");

        sweep_prefix(&prefix, Some(spared.id()));
        thread::sleep(Duration::from_millis(200));
        assert!(
            matches!(spared.try_wait(), Ok(None)),
            "the sweep killed the process it was told to spare"
        );

        let _ = spared.kill();
        let _ = spared.wait();
    }

    #[test]
    fn reads_a_resolution_or_falls_back_to_the_default() {
        assert_eq!(parse_res("800x600"), Some((800, 600)));
        assert_eq!(parse_res(" 1280 X 720 "), Some((1280, 720)));
        assert_eq!(parse_res("640"), None);
        assert_eq!(parse_res("wide x tall"), None);
        assert_eq!(parse_res(DEFAULT_RES), Some((800, 600)));
    }

    /// The config has to survive whatever the metadata says, since it comes
    /// from a database line or the command line and neither is checked.
    #[test]
    fn a_broken_resolution_still_gives_a_usable_config() {
        let exe = std::env::current_exe().expect("this test binary");
        let meta = HashMap::from([(META_RES.to_string(), "huge".to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!((cfg.width, cfg.height), (800, 600));

        let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!((cfg.width, cfg.height), (1024, 768));

        // An empty value is nothing said, not a broken resolution.
        let meta = HashMap::from([(META_RES.to_string(), String::new())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!((cfg.width, cfg.height), (800, 600));
        assert_eq!(cfg.dialog, Dialog::Drive);
    }

    /// `wine_res=pick` hands the dialog to whoever is watching: no driver in
    /// the command at all, and a session big enough for whatever they choose.
    #[test]
    fn pick_leaves_the_dialog_alone() {
        let exe = std::env::current_exe().expect("this test binary");
        for spelling in ["pick", "PICK", "  Pick  "] {
            let meta = HashMap::from([(META_RES.to_string(), spelling.to_string())]);
            let cfg = Config::from_meta(&exe, &meta).unwrap();
            assert_eq!(cfg.dialog, Dialog::Pick, "{spelling:?}");
            assert_eq!((cfg.width, cfg.height), (1920, 1200), "{spelling:?}");

            // The driver is still the command - it is what starts the demo
            // and what reports its end - but it is told to press nothing and
            // to leave the demo's window alone.
            let args = cfg.wine_args(Some(Path::new("/sys/win/autodlg.exe")));
            assert_eq!(args[0], "/sys/win/autodlg.exe", "{spelling:?}");
            assert!(args.contains(&"--no-go".to_string()), "{spelling:?}");
            assert!(args.contains(&"--no-fill".to_string()), "{spelling:?}");
            // Nothing chosen and nothing ticked: those are the dialog's own.
            assert!(!args.contains(&"--prefer".to_string()), "{spelling:?}");
            assert!(!args.contains(&"--check".to_string()), "{spelling:?}");
            let launch = args.iter().position(|a| a == "--launch").expect("--launch");
            assert_eq!(args[launch + 1], cfg.exe.to_string_lossy(), "{spelling:?}");
        }

        // Any other value is still a resolution, and still driven.
        let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!(cfg.dialog, Dialog::Drive);
        let args = cfg.wine_args(Some(Path::new("/a.exe")));
        assert!(args.contains(&"--prefer".to_string()));
        assert!(!args.contains(&"--no-go".to_string()));
    }

    /// The demo has to be started *by* the driver, in the same session, or the
    /// driver's `EnumWindows` never sees the dialog it exists to answer.
    #[test]
    fn the_driver_launches_the_demo() {
        let exe = std::env::current_exe().expect("this test binary");
        let cfg = Config::from_meta(&exe, &HashMap::new()).unwrap();
        let args = cfg.wine_args(Some(Path::new("/sys/win/autodlg.exe")));

        // No virtual desktop unless an entry asks for one: wine runs the
        // driver directly.
        assert!(!cfg.desktop);
        assert_eq!(args[0], "/sys/win/autodlg.exe");
        let launch = args.iter().position(|a| a == "--launch").expect("--launch");
        assert_eq!(args[launch + 1], cfg.exe.to_string_lossy());
        // The size demarc runs at is the size the dialog gets told to pick.
        let prefer = args.iter().position(|a| a == "--prefer").expect("--prefer");
        assert_eq!(args[prefer + 1], "800x600");

        // Without a driver the demo is the one command.
        let bare = cfg.wine_args(None);
        assert_eq!(bare, vec![cfg.exe.to_string_lossy().into_owned()]);
    }

    /// `wine_desktop=true` wraps whatever would have run in a wine virtual
    /// desktop the size of the session. A handful of demos - Equinox's *Kings
    /// of the Playground* among them - do not survive a real display mode
    /// change under gamescope's Xwayland, and this is what saves them.
    #[test]
    fn a_virtual_desktop_wraps_the_command_when_asked_for() {
        let exe = std::env::current_exe().expect("this test binary");
        let driver = Path::new("/sys/win/autodlg.exe");

        for spelling in ["true", "1", "YES", " on "] {
            let meta = HashMap::from([(META_DESKTOP.to_string(), spelling.to_string())]);
            let cfg = Config::from_meta(&exe, &meta).unwrap();
            assert!(cfg.desktop, "{spelling:?}");

            let args = cfg.wine_args(Some(driver));
            assert_eq!(args[0], "explorer", "{spelling:?}");
            assert_eq!(args[1], "/desktop=demarc,800x600", "{spelling:?}");
            // Everything the desktop hosts is still one command: the driver,
            // which starts the demo itself.
            assert_eq!(args[2], "/sys/win/autodlg.exe", "{spelling:?}");
        }

        // The desktop is the size of the session, whatever that turned out to
        // be - including a `pick` session, which is why it is big.
        let meta = HashMap::from([
            (META_DESKTOP.to_string(), "true".to_string()),
            (META_RES.to_string(), PICK.to_string()),
        ]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        let args = cfg.wine_args(Some(driver));
        assert_eq!(args[1], "/desktop=demarc,1920x1200");
        // ...and it hosts the driver even with the dialog left alone, since
        // the driver is what starts the demo inside it.
        assert_eq!(args[2], "/sys/win/autodlg.exe");
        assert!(args.contains(&"--no-go".to_string()));

        // Anything else is a no, including nonsense and an empty value.
        for spelling in ["false", "no", "0", "", "maybe"] {
            let meta = HashMap::from([(META_DESKTOP.to_string(), spelling.to_string())]);
            let cfg = Config::from_meta(&exe, &meta).unwrap();
            assert!(!cfg.desktop, "{spelling:?}");
            assert_ne!(cfg.wine_args(Some(driver))[0], "explorer", "{spelling:?}");
        }
    }
}
