//! Windows demo backend: a PE executable run under wine inside a headless
//! [gamescope], with everything it renders captured back into demarc.
//!
//! Unlike every other backend here, this one cannot be stepped. wine is a live
//! real-time producer with no notion of "advance one frame", so the captured
//! audio stream is the clock instead: [`WineEmu::run`] takes whatever samples
//! have arrived since the last call and pairs them with the newest composited
//! frame. Frame duplication and dropping then fall out of the frontend's
//! existing audio-buffer pacing (`AUDIO_BUF_MIN`/`AUDIO_BUF_MAX` in
//! [`crate::emulator`]) rather than being fought: the extra `run` the frontend
//! issues when the sink runs dry simply finds no new samples, which is the
//! truth, and cannot run the demo fast the way it could a real core.
//!
//! The pipeline, none of which touches the user's screen:
//!
//! ```text
//! gamescope --backend headless -- wine demo.exe     (Vulkan on the real GPU)
//!   |- video: gamescope's PipeWire node -> gst-launch -> raw RGBA on a pipe
//!   `- audio: PULSE_SINK null sink -> pw-record -> raw s16 on a pipe
//! ```
//!
//! gamescope is what makes this worth doing over an Xvfb-style virtual display:
//! it composites with Vulkan on the real GPU, so wine's D3D/OpenGL translation
//! stays hardware-accelerated instead of falling back to llvmpipe. It also pins
//! the output size (`-W`/`-H`) and the frame cadence (`-r`), which is what lets
//! [`Backend::get_frame_size`] and [`Backend::fps`] return fixed values for a
//! source that is otherwise entirely at the demo's mercy.
//!
//! Nearly every PC demo opens with a setup dialog before it renders anything,
//! and a headless gamescope has no input devices to dismiss it with. So
//! `system/win/demarc-autodlg.exe` runs inside the same wine prefix and answers
//! the dialog through Win32 messages, walking the control tree rather than
//! clicking pixels. That is also how the capture size gets pinned end to end,
//! since that dialog is usually where a demo's resolution is chosen — and how
//! Fullscreen gets ticked, which inside the virtual desktop is what gives a
//! clean full-frame capture. See [`Config::command`] for why there is a virtual
//! desktop at all; it is the difference between a demo that runs and one that
//! dies a few seconds in with its last frame frozen on screen.
//!
//! [gamescope]: https://github.com/ValveSoftware/gamescope

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, info, warn};

use crate::frontend::system_dir;
use crate::retro_emu::Backend;

/// Capture rate handed to the frontend, matching the other backends so the
/// existing [`crate::audio::AudioResampler`] converts to the device rate.
const SAMPLE_RATE: u32 = 44100;

const DEFAULT_WIDTH: u32 = 640;
const DEFAULT_HEIGHT: u32 = 480;
/// gamescope holds this cadence closely — measured 49.9fps at `-r 50` and
/// 58.8fps at `-r 60`, the shortfall at 60 being frames the demo itself missed.
const DEFAULT_FPS: u32 = 60;

/// How long the dialog driver keeps looking before giving up. Generous: a cold
/// wine prefix can take a while to get the first window up.
const DIALOG_TIMEOUT: f64 = 20.0;

/// One second of stereo capture. The producer is real time and so is the
/// consumer, so this only fills if the frontend stalls — at which point the
/// oldest samples are the ones worth losing.
const AUDIO_CAP: usize = SAMPLE_RATE as usize * 2;

/// Read granularity for the audio pipe: ~12ms, small enough not to add
/// meaningful latency, large enough to keep syscalls off the hot path.
const AUDIO_CHUNK: usize = 2048;

/// How long to wait for gamescope to announce its capture node before giving up
/// on the session.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

/// The dialog driver, relative to [`system_dir`].
const AUTODLG: &str = "win/demarc-autodlg.exe";

/// External programs a session needs, named here so a missing one produces one
/// clear error at load time instead of a confusing failure three steps later.
const REQUIRED_TOOLS: [&str; 5] = ["gamescope", "wine", "gst-launch-1.0", "pw-record", "pactl"];

/// Names this backend's capture sinks, so [`sweep_stale_sinks`] can tell them
/// from every other null sink on the system.
const SINK_PREFIX: &str = "demarc_pc_";

/// Distinguishes concurrent sessions' null sinks. A grid can have several PC
/// demos running at once, and each needs its own sink to capture in isolation.
static SESSION_SEQ: AtomicU32 = AtomicU32::new(0);

/// Mutable counterpart to [`crate::retro_emu::frame_bytes`] — the destination
/// the capture pipe is read into. Same always-sound width-narrowing view: one
/// packed RGBA8 pixel per `u32`, bytes already in `[r, g, b, a]` memory order,
/// which is exactly what `videoconvert`'s `RGBA` output hands over.
fn frame_bytes_mut(pixels: &mut [u32]) -> &mut [u8] {
    let len = std::mem::size_of_val(pixels);
    unsafe { std::slice::from_raw_parts_mut(pixels.as_mut_ptr().cast::<u8>(), len) }
}

fn has_tool(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
        .unwrap_or(false)
}

/// How a session is set up, resolved once from the entry's metadata so
/// [`Backend::reset`] can rebuild an identical one.
#[derive(Clone)]
struct Config {
    exe: PathBuf,
    width: u32,
    height: u32,
    fps: u32,
    /// Controls for the dialog driver to select before starting the demo, each
    /// matched as a substring of the control's label. Defaults to the capture
    /// size, which is how most demos label their resolution radios.
    prefer: Vec<String>,
    /// Labels that count as "start the demo". Empty leaves the driver's own
    /// default set (RUN, OK, START, GO, LAUNCH, PLAY, YES).
    go: Vec<String>,
    /// Checkboxes to force on before starting, matched by label.
    check: Vec<String>,
    /// Checkboxes to force off.
    uncheck: Vec<String>,
}

/// Turned *on* by default on any demo that offers it.
///
/// This looks backwards for a capture and isn't. Inside the virtual desktop,
/// "fullscreen" means the size of that desktop — exactly the capture size, no
/// window decoration, nothing else on screen. Windowed means a real decorated
/// window inset on the wine desktop background, which puts a Windows title bar
/// in the middle of the captured frame (measured: We Cell renders its title bar
/// across the top 40 pixels when this is off). The mode switch that makes
/// fullscreen dangerous outside a virtual desktop is contained by being inside
/// one.
const DEFAULT_CHECK: &str = "Fullscreen";

impl Config {
    fn from_meta(exe: &Path, meta: &HashMap<String, String>) -> Result<Self> {
        let get = |key: &str| meta.get(key).map(String::as_str);
        let num = |key: &str, def: u32| -> u32 {
            get(key).and_then(|v| v.parse().ok()).unwrap_or(def)
        };
        let width = num("pc_width", DEFAULT_WIDTH);
        let height = num("pc_height", DEFAULT_HEIGHT);
        let list = |key: &str| -> Vec<String> {
            get(key)
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut prefer = list("pc_dialog");
        if prefer.is_empty() {
            prefer = vec![format!("{width}x{height}")];
        }
        Ok(Self {
            // wine resolves a Unix path fine, but it has to be absolute: the
            // inner shell starts in whatever directory demarc happens to be in.
            exe: exe
                .canonicalize()
                .with_context(|| format!("No such executable: {}", exe.display()))?,
            width,
            height,
            fps: num("pc_fps", DEFAULT_FPS),
            prefer,
            go: list("pc_go"),
            // Explicit metadata wins even when it is empty, which is how a demo
            // whose fullscreen path is broken opts out: `pc_check=`.
            check: if meta.contains_key("pc_check") {
                list("pc_check")
            } else {
                vec![DEFAULT_CHECK.to_string()]
            },
            uncheck: list("pc_uncheck"),
        })
    }

    fn frame_len(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// The arguments to `wine` that run this demo, dialog and all.
    ///
    /// Everything happens inside `explorer /desktop=`, a wine virtual desktop
    /// fixed at the capture size. That containment is what keeps demos alive:
    /// one that switches display modes on its way to fullscreen — which is most
    /// of them — otherwise tears down and remaps its X window under gamescope's
    /// Xwayland and dies of `BadWindow` on `X_UnmapWindow` seconds in, leaving
    /// its last frame frozen on screen. Inside the desktop the mode switch is
    /// wine's own business and never reaches X. (Measured on Equinox's *Kings
    /// of the Playground*: frozen after 101 frames without, still running at
    /// 2068 frames in 35s with.)
    ///
    /// A virtual desktop hosts exactly one command, which is why the driver
    /// starts the demo itself via `--launch` rather than being a second process
    /// alongside it: a child inherits the desktop, a sibling would not, and the
    /// driver has to be *in* the desktop for `EnumWindows` to see the dialog.
    /// Waiting on the demo is then also the driver's job, so this command's
    /// lifetime is the demo's lifetime.
    ///
    /// No shell is involved, so no argument needs quoting — demo filenames are
    /// full of spaces, brackets and apostrophes.
    fn command(&self, autodlg: &Path) -> Vec<String> {
        let mut args = vec![
            "explorer".to_string(),
            format!("/desktop=demarc,{}x{}", self.width, self.height),
            autodlg.to_string_lossy().into_owned(),
            "--launch".into(),
            self.exe.to_string_lossy().into_owned(),
            "--timeout".into(),
            DIALOG_TIMEOUT.to_string(),
        ];
        for prefer in &self.prefer {
            args.push("--prefer".into());
            args.push(prefer.clone());
        }
        if !self.go.is_empty() {
            args.push("--go".into());
            args.push(self.go.join(","));
        }
        for (flag, labels) in [("--check", &self.check), ("--uncheck", &self.uncheck)] {
            for label in labels {
                args.push(flag.into());
                args.push(label.clone());
            }
        }
        args
    }
}

/// Handover point between the video reader thread and [`WineEmu::run`].
///
/// Newest frame wins — there is no queue, because there is nothing to be gained
/// by showing a stale frame from a source that is already running in real time.
#[derive(Default)]
struct VideoSlot {
    /// Newest complete frame, taken by `run`.
    ready: Option<Vec<u32>>,
    /// A buffer handed back for the reader to fill next, so the steady state
    /// allocates nothing. Costs one spare frame of memory.
    spare: Option<Vec<u32>>,
    /// Bumped per captured frame; drives [`Backend::frame_hash`].
    serial: u64,
}

/// What [`Session::start`] learns by watching gamescope's stderr.
enum Announced {
    /// The PipeWire node id the composited output is published on. gamescope
    /// prints it, so it never has to be guessed or looked up by name.
    Node(u32),
    /// The nested X display it started for wine, e.g. `:1`.
    Display(String),
}

/// One running demo: the process tree, its null sink, and the threads draining
/// the capture pipes. Dropping it tears all of that down.
struct Session {
    gamescope: Child,
    video: Option<Child>,
    audio: Option<Child>,
    /// PipeWire module id of this session's null sink, for unloading it again.
    sink_module: String,
    /// Nested X display wine is on, used for best-effort key injection.
    display: Option<String>,
    readers: Vec<JoinHandle<()>>,
}

impl Session {
    fn start(
        cfg: &Config,
        video: &Arc<Mutex<VideoSlot>>,
        audio: &Arc<Mutex<VecDeque<i16>>>,
    ) -> Result<Self> {
        if let Some(missing) = REQUIRED_TOOLS.iter().find(|t| !has_tool(t)) {
            bail!(
                "Running Windows demos needs `{missing}`, which is not on PATH \
                 (this backend needs gamescope, wine, gstreamer and pipewire)"
            );
        }
        let autodlg = system_dir().join(AUTODLG);
        if !autodlg.is_file() {
            bail!("Dialog driver missing from the system dir: {}", autodlg.display());
        }

        sweep_stale_sinks();
        let sink = format!(
            "{SINK_PREFIX}{}_{}",
            std::process::id(),
            SESSION_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let sink_module = load_null_sink(&sink)?;

        let inner = cfg.command(&autodlg);
        debug!("wine session: wine {}", inner.join(" "));

        let mut gamescope = Command::new("gamescope");
        let gamescope = gamescope
            .args([
                "--backend",
                "headless",
                "-W",
                &cfg.width.to_string(),
                "-H",
                &cfg.height.to_string(),
                "-r",
                &cfg.fps.to_string(),
                "--",
                "wine",
            ])
            .args(&inner)
            // Everything wine plays lands here and nowhere near the user's
            // speakers, and nothing else on the desktop lands in our capture.
            // Everything runs from the demo's own directory. A release that
            // ships a `data/` folder or its own `fmod.dll` finds neither when
            // started from wherever demarc happens to have been launched, and
            // fails silently — a dialog that works and then a black screen.
            .current_dir(cfg.exe.parent().unwrap_or(Path::new(".")))
            .env("PULSE_SINK", &sink)
            .env("WINEDEBUG", std::env::var("WINEDEBUG").unwrap_or("-all".into()))
            .stdin(Stdio::null())
            // The dialog driver reports what it found and did on stdout, which
            // arrives here by way of gamescope. It is the only window onto what
            // happened inside the virtual desktop, so it goes to the log rather
            // than to /dev/null.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let gamescope = capture_child(gamescope)
            .spawn()
            .context("Failed to start gamescope")?;

        // Built now, before anything else can fail, so every `?` below unwinds
        // through `Drop` and cleans up the process tree and the null sink.
        let mut session = Session {
            gamescope,
            video: None,
            audio: None,
            sink_module,
            display: None,
            readers: Vec::new(),
        };

        let node = session.watch_gamescope()?;
        session.video = Some(session_video(cfg, node, video, &mut session.readers)?);
        session.audio = Some(session_audio(&sink, audio, &mut session.readers)?);
        info!(
            "Started {} under gamescope: {}x{} @{}Hz, pipewire node {node}",
            cfg.exe.display(),
            cfg.width,
            cfg.height,
            cfg.fps
        );
        Ok(session)
    }

    /// Drain gamescope's stderr on a thread — it is chatty enough to fill the
    /// pipe and deadlock itself otherwise — and wait for it to announce the
    /// capture node. The nested display goes past on the way, so it is picked
    /// up here too rather than looked up separately.
    fn watch_gamescope(&mut self) -> Result<u32> {
        let stderr = self
            .gamescope
            .stderr
            .take()
            .ok_or_else(|| anyhow!("gamescope stderr not captured"))?;
        let (tx, rx) = channel::<Announced>();
        self.readers.push(
            thread::Builder::new()
                .name("wine-emu-log".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if let Some(rest) = line.split("stream available on node ID: ").nth(1) {
                            if let Ok(id) = rest.trim().parse::<u32>() {
                                let _ = tx.send(Announced::Node(id));
                            }
                        } else if let Some(rest) = line.split("Starting Xwayland on ").nth(1) {
                            let _ = tx.send(Announced::Display(rest.trim().to_string()));
                        }
                        debug!(target: "gamescope", "{line}");
                    }
                })?,
        );

        if let Some(stdout) = self.gamescope.stdout.take() {
            self.readers.push(
                thread::Builder::new()
                    .name("wine-emu-dlg".into())
                    .spawn(move || {
                        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                            debug!(target: "autodlg", "{line}");
                        }
                    })?,
            );
        }

        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Announced::Node(id)) => return Ok(id),
                Ok(Announced::Display(d)) => self.display = Some(d),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        bail!(
            "gamescope never announced a PipeWire capture node — it has to be \
             built with PipeWire support for this backend to see anything"
        )
    }

    /// Best-effort key injection into the nested X server. XTEST reaches the
    /// server, but whether the demo consumes what it delivers depends on the
    /// demo, so this is deliberately fire-and-forget.
    fn send_key(&self, keysym: &str) {
        let Some(display) = &self.display else { return };
        if !has_tool("xdotool") {
            return;
        }
        let _ = Command::new("xdotool")
            .args(["key", "--clearmodifiers", keysym])
            .env("DISPLAY", display)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Children first: killing the capture ends the reader threads by EOF.
        for child in [self.video.as_mut(), self.audio.as_mut()].into_iter().flatten() {
            kill_group(child);
        }
        kill_group(&mut self.gamescope);
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        unload_module(&self.sink_module);
    }
}

/// SIGKILL a child's whole process group. [`capture_child`] gives each one its
/// own group, so the group is exactly that child and everything it spawned.
fn kill_group(child: &mut Child) {
    let pid = child.id() as i32;
    // Negative pid: signal the group. Safe by inspection — `kill` touches no
    // memory, and the group is one we created.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let _ = child.wait();
}

/// Configure a capture child before spawning it.
///
/// Two things, both about teardown. Its own process group, so [`kill_group`]
/// can take the whole tree — gamescope leaves a `gamescopereaper` and the
/// entire wine process tree behind if only gamescope itself is signalled. And
/// `PR_SET_PDEATHSIG`, so a demarc that dies without running [`Session::drop`]
/// takes its capture with it: closing the pipes kills whichever child is
/// mid-write, but one whose source has merely gone quiet — a `gst-launch` whose
/// gamescope is already gone — would otherwise sit there forever.
fn capture_child(cmd: &mut Command) -> &mut Command {
    // SAFETY: `pre_exec` runs between fork and exec, so it may only call
    // async-signal-safe functions. `prctl` is a bare syscall.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.process_group(0)
}

fn load_null_sink(name: &str) -> Result<String> {
    let out = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={name}"),
            "sink_properties=device.description=demarc",
        ])
        .output()
        .context("Failed to run pactl")?;
    if !out.status.success() {
        bail!(
            "pactl could not create a capture sink: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Unload capture sinks left behind by a demarc that died without running
/// [`Session::drop`] — killed, crashed or SIGTERMed.
///
/// The capture *processes* need no such sweep: they all write into a pipe this
/// process holds the other end of, so they take a SIGPIPE and exit the moment
/// it goes away. A null sink lives in the PipeWire daemon instead, with nothing
/// tying it to our lifetime, so without this it would accumulate one dead sink
/// per crash.
///
/// The owning pid is in the sink's name, which makes "stale" unambiguous: a
/// sink whose pid is gone can never come back, and one belonging to a live
/// demarc — this instance or a second one — is left alone. Should a dead pid
/// have been recycled, the sink is merely swept later instead of wrongly now.
fn sweep_stale_sinks() {
    let Ok(out) = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    else {
        return;
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((id, pid)) = stale_sink(line) else {
            continue;
        };
        debug!("Unloading capture sink left by dead demarc {pid}");
        unload_module(id);
    }
}

/// Parse one `pactl list short modules` row, yielding the module id and owning
/// pid if it is one of our capture sinks and that pid is no longer running.
fn stale_sink(line: &str) -> Option<(&str, u32)> {
    let mut cols = line.split('\t');
    let id = cols.next()?;
    if cols.next()? != "module-null-sink" {
        return None;
    }
    let name = cols
        .next()?
        .split_whitespace()
        .find_map(|arg| arg.strip_prefix("sink_name="))?;
    let pid: u32 = name
        .strip_prefix(SINK_PREFIX)?
        .split('_')
        .next()?
        .parse()
        .ok()?;
    (!Path::new(&format!("/proc/{pid}")).exists()).then_some((id, pid))
}

fn unload_module(id: &str) {
    if id.is_empty() {
        return;
    }
    let _ = Command::new("pactl")
        .args(["unload-module", id])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Start the video capture and the thread that reads whole frames off it.
fn session_video(
    cfg: &Config,
    node: u32,
    slot: &Arc<Mutex<VideoSlot>>,
    readers: &mut Vec<JoinHandle<()>>,
) -> Result<Child> {
    // gst-launch is what bridges PipeWire to a plain pipe; ffmpeg has no
    // PipeWire video input. The caps pin the format so the reader can treat the
    // stream as fixed-size frames with no parsing at all.
    let mut child = Command::new("gst-launch-1.0");
    let child = child
        .args([
            "-q",
            "pipewiresrc",
            &format!("path={node}"),
            "!",
            "videoconvert",
            "!",
            &format!(
                "video/x-raw,format=RGBA,width={},height={}",
                cfg.width, cfg.height
            ),
            "!",
            "fdsink",
            "fd=1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = capture_child(child)
        .spawn()
        .context("Failed to start the video capture pipeline")?;

    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("video capture stdout not captured"))?;
    let slot = slot.clone();
    let frame_len = cfg.frame_len();
    readers.push(
        thread::Builder::new()
            .name("wine-emu-video".into())
            .spawn(move || {
                loop {
                    let mut buf = slot
                        .lock()
                        .ok()
                        .and_then(|mut s| s.spare.take())
                        .unwrap_or_default();
                    buf.resize(frame_len, 0);
                    // Blocks until a whole frame is through; returns at EOF,
                    // which is how this thread ends when the session is torn down.
                    if out.read_exact(frame_bytes_mut(&mut buf)).is_err() {
                        return;
                    }
                    let Ok(mut slot) = slot.lock() else { return };
                    // A frame the frontend never collected becomes the next
                    // spare, so a slow consumer costs no allocations either.
                    if let Some(dropped) = slot.ready.replace(buf) {
                        slot.spare = Some(dropped);
                    }
                    slot.serial += 1;
                }
            })?,
    );
    Ok(child)
}

/// Start the audio capture and the thread that reads samples off it.
fn session_audio(
    sink: &str,
    buffer: &Arc<Mutex<VecDeque<i16>>>,
    readers: &mut Vec<JoinHandle<()>>,
) -> Result<Child> {
    let mut child = Command::new("pw-record");
    let child = child
        .args([
            "--target",
            &format!("{sink}.monitor"),
            "--raw",
            "--format",
            "s16",
            "--rate",
            &SAMPLE_RATE.to_string(),
            "--channels",
            "2",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = capture_child(child)
        .spawn()
        .context("Failed to start the audio capture")?;

    let mut out = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("audio capture stdout not captured"))?;
    let buffer = buffer.clone();
    readers.push(
        thread::Builder::new()
            .name("wine-emu-audio".into())
            .spawn(move || {
                // Read in whole sample pairs so no frame is ever split across
                // reads and the interleaving cannot drift.
                let mut bytes = [0u8; AUDIO_CHUNK];
                loop {
                    if out.read_exact(&mut bytes).is_err() {
                        return;
                    }
                    let Ok(mut buffer) = buffer.lock() else { return };
                    buffer.extend(
                        bytes
                            .chunks_exact(2)
                            .map(|b| i16::from_le_bytes([b[0], b[1]])),
                    );
                    // Only reachable if the frontend stops consuming; keep the
                    // newest audio rather than falling further behind.
                    let over = buffer.len().saturating_sub(AUDIO_CAP);
                    if over > 0 {
                        buffer.drain(..over);
                    }
                }
            })?,
    );
    Ok(child)
}

/// A Windows demo running under wine, presented to the frontend as a backend.
pub struct WineEmu {
    cfg: Config,
    /// `None` only if a [`Backend::reset`] failed to bring a new one up.
    session: Option<Session>,
    slot: Arc<Mutex<VideoSlot>>,
    incoming: Arc<Mutex<VecDeque<i16>>>,

    /// Latest captured frame, refreshed by `run`.
    frame: Vec<u32>,
    /// Accumulates across `run` calls; drained by `with_audio`.
    audio: Vec<i16>,
    /// [`VideoSlot::serial`] as of the last frame taken.
    serial: u64,
}

impl WineEmu {
    pub fn new(exe: &Path, meta: HashMap<String, String>) -> Result<Self> {
        let cfg = Config::from_meta(exe, &meta)?;
        let slot = Arc::new(Mutex::new(VideoSlot::default()));
        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        let session = Session::start(&cfg, &slot, &incoming)
            .with_context(|| format!("Failed to run {}", cfg.exe.display()))?;
        Ok(Self {
            frame: vec![0u32; cfg.frame_len()],
            cfg,
            session: Some(session),
            slot,
            incoming,
            audio: Vec::new(),
            serial: 0,
        })
    }
}

impl Backend for WineEmu {
    /// Collect whatever the capture produced since the last call. Never blocks:
    /// the demo runs whether or not anyone asks it to, so an early call simply
    /// finds nothing new, and that is the honest answer.
    fn run(&mut self) -> bool {
        if let Ok(mut slot) = self.slot.lock()
            && slot.serial != self.serial
            && let Some(frame) = slot.ready.take()
        {
            // Hand our old buffer back for the reader to fill next.
            slot.spare = Some(std::mem::replace(&mut self.frame, frame));
            self.serial = slot.serial;
        }
        if let Ok(mut incoming) = self.incoming.lock() {
            self.audio.extend(incoming.drain(..));
        }
        self.session.is_some()
    }

    fn frame_hash(&self) -> u64 {
        self.serial
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(self.cfg.width as usize, self.cfg.height as usize, &self.frame);
    }

    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.audio);
        self.audio.clear();
    }

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
        SAMPLE_RATE as f64
    }

    /// What gamescope was asked to pace at, which it holds closely — the demo
    /// missing frames shows up as repeated captures, not as a changed rate.
    fn fps(&self) -> f64 {
        self.cfg.fps as f64
    }

    /// Relaunch the demo from scratch: there is no state to rewind, and a demo
    /// is short enough that starting over is what a reset means anyway.
    fn reset(&mut self) {
        self.session = None;
        match Session::start(&self.cfg, &self.slot, &self.incoming) {
            Ok(session) => self.session = Some(session),
            Err(e) => warn!("Failed to restart {}: {e:#}", self.cfg.exe.display()),
        }
    }

    /// Best effort, and only on press — `xdotool key` is a press and a release,
    /// so a separate release would double up. See [`Session::send_key`].
    fn press_key(&mut self, code: u32, down: bool, _mods: u16) {
        if !down {
            return;
        }
        if let (Some(session), Some(keysym)) = (&self.session, keysym(code)) {
            session.send_key(&keysym);
        }
    }

    // A live capture has nothing to skip to and no disks or joypads. The mouse
    // could go the same way as `press_key` if a demo ever turns out to need it.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        0
    }
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn skip_frames(&mut self, _frames: u32) {}

    fn get_info(&self) -> Option<String> {
        Some(format!(
            "PC demo {}x{} @{}Hz (wine + gamescope)",
            self.cfg.width, self.cfg.height, self.cfg.fps
        ))
    }
}

/// libretro keycode to X keysym name, for the handful of keys a demo reacts to.
/// `retro_key` follows SDL, which is plain ASCII across the printable range.
fn keysym(code: u32) -> Option<String> {
    match code {
        13 => Some("Return".into()),
        27 => Some("Escape".into()),
        32 => Some("space".into()),
        c if (0x30..=0x39).contains(&c) || (0x61..=0x7a).contains(&c) => {
            Some((c as u8 as char).to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo runs inside a virtual desktop sized to the capture, started by
    /// the driver rather than beside it.
    #[test]
    fn command_wraps_the_demo_in_a_virtual_desktop() {
        let cfg = Config::from_meta(Path::new("."), &HashMap::new()).unwrap();
        let args = cfg.command(Path::new("/sys/autodlg.exe"));
        assert_eq!(args[0], "explorer");
        assert_eq!(args[1], "/desktop=demarc,640x480");
        assert_eq!(args[2], "/sys/autodlg.exe");
        assert_eq!(args[3], "--launch");
        // Nothing is shell-quoted, because nothing goes through a shell.
        assert!(args.iter().all(|a| !a.contains('\'')));
        assert!(args.windows(2).any(|w| w == ["--check", "Fullscreen"]));
    }

    /// The capture size doubles as the dialog answer, since that is how demos
    /// label their resolution radios.
    #[test]
    fn default_dialog_answer_is_the_capture_size() {
        let cfg = Config::from_meta(Path::new("."), &HashMap::new()).unwrap();
        assert_eq!(cfg.prefer, vec!["640x480".to_string()]);
        assert_eq!((cfg.width, cfg.height, cfg.fps), (640, 480, 60));
    }

    /// Fullscreen goes on by default — inside a virtual desktop that is the
    /// clean full-frame capture — and `pc_check=` is how a demo whose
    /// fullscreen path is broken opts out.
    #[test]
    fn fullscreen_is_checked_unless_metadata_says_otherwise() {
        let cfg = Config::from_meta(Path::new("."), &HashMap::new()).unwrap();
        assert_eq!(cfg.check, vec!["Fullscreen".to_string()]);

        let opt_out: HashMap<String, String> =
            [("pc_check".to_string(), String::new())].into_iter().collect();
        let cfg = Config::from_meta(Path::new("."), &opt_out).unwrap();
        assert!(cfg.check.is_empty());
    }

    #[test]
    fn meta_overrides_size_and_dialog() {
        let meta: HashMap<String, String> = [
            ("pc_width", "800"),
            ("pc_height", "600"),
            ("pc_fps", "50"),
            ("pc_dialog", "800x600, FSAA 2x"),
            ("pc_go", "START,GO"),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let cfg = Config::from_meta(Path::new("."), &meta).unwrap();
        assert_eq!(cfg.prefer, vec!["800x600".to_string(), "FSAA 2x".to_string()]);
        assert_eq!(cfg.go, vec!["START".to_string(), "GO".to_string()]);
        assert_eq!((cfg.width, cfg.height, cfg.fps), (800, 600, 50));
    }

    /// Only our own sinks, only dead ones, and only when the row really is a
    /// null sink — everything else on the system has to be left alone.
    #[test]
    fn only_our_own_dead_sinks_are_swept() {
        let live = std::process::id();
        // A pid that cannot be running: pid 0 is the scheduler, never in /proc.
        let dead = format!("7\tmodule-null-sink\tsink_name={SINK_PREFIX}0_3 sink_properties=x");
        assert_eq!(stale_sink(&dead), Some(("7", 0)));

        let ours_alive =
            format!("7\tmodule-null-sink\tsink_name={SINK_PREFIX}{live}_0 sink_properties=x");
        assert_eq!(stale_sink(&ours_alive), None);

        let someone_elses = "7\tmodule-null-sink\tsink_name=my_speakers";
        assert_eq!(stale_sink(someone_elses), None);

        let not_a_sink = format!("7\tmodule-loopback\tsink_name={SINK_PREFIX}0_3");
        assert_eq!(stale_sink(&not_a_sink), None);
    }

    #[test]
    fn frame_bytes_mut_views_every_channel() {
        let mut pixels = vec![0u32; 4];
        let bytes = frame_bytes_mut(&mut pixels);
        assert_eq!(bytes.len(), 16);
        bytes[0] = 0xff;
        assert_eq!(pixels[0] & 0xff, 0xff);
    }
}
