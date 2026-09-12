//! One throwaway wine prefix per demo, made with [bubblewrap].
//!
//! Both Windows backends run out of a single prefix, `~/.wine-demarc`, and that
//! one prefix is what keeps them to one demo at a time. Two reasons, and they
//! are separate:
//!
//! - **One wineserver.** wine names its server socket after the *device and
//!   inode* of the prefix (`/tmp/.wine-<uid>/server-<dev>-<ino>`), so everything
//!   pointed at the same directory joins the same server. `wineserver -k` is
//!   then all-or-nothing: closing one demo closes every demo. That is exactly
//!   what [`crate::wine::close_prefix`] does, what the gamescope core's
//!   `StopWineServer` does, and why `docs/GAMESCOPE.md` lists "two Windows demos
//!   at once is out".
//! - **One set of files.** A demo is free to write to the prefix — registry
//!   keys, a config file in `drive_c`, a font it installs on the way past — and
//!   two of them doing it at once are writing over each other. The provisioning
//!   in `just wine-prefix` (DXVK, the native `d3dx9`, the registry tweaks) is
//!   also there to be preserved, and every demo that runs is another chance to
//!   damage it.
//!
//! What this module does about both is give each session its own prefix without
//! copying one. `bwrap` mounts an overlay whose lower layer is the real prefix
//! and whose upper layer is an invisible tmpfs, at a path nothing else uses:
//!
//! ```text
//! bwrap --dev-bind / /                       # the host, as it is
//!       --proc /proc --unshare-pid           # a pid namespace of its own
//!       --die-with-parent
//!       --perms 0700 --tmpfs /tmp/.wine-1000 # wine's socket directory, private
//!       --overlay-src ~/.wine-demarc
//!       --tmp-overlay /run/user/1000/demarc-wine-1000/4711/0
//!       --setenv WINEPREFIX /run/user/1000/demarc-wine-1000/4711/0
//!       -- wine demarc-autodlg.exe --launch demo.exe ...
//! ```
//!
//! Reads come through from the real prefix, so the session starts fully
//! provisioned; writes land in the tmpfs and are gone when the sandbox is. The
//! path is unique, so nothing keys on it twice, and the private `/tmp/.wine-<uid>`
//! is what actually guarantees a private wineserver — the overlay's device
//! number would very likely differ too, but "very likely" is not a thing to hang
//! process isolation on.
//!
//! The pid namespace is the other half, and it is worth as much as the prefix
//! is. wine's services (`wineserver`, `services.exe`, `winedevice.exe`) call
//! `setsid` and leave the process group, which is why both backends carry code
//! to hunt them down afterwards — [`crate::wine::sweep_prefix`] exists
//! because thirty-seven of them had piled up. Inside a pid namespace there is
//! nowhere to escape to: when the demo (pid 1 in there) exits, the kernel takes
//! the rest of the namespace with it, and so does `--die-with-parent` if the
//! session is killed from outside instead. Nothing is left to sweep, and nothing
//! that ends one demo can reach another.
//!
//! This is **not** a security boundary and is not meant as one: `--dev-bind / /`
//! hands the demo the whole host filesystem, because it needs the GPU nodes, the
//! audio socket, the X socket gamescope just made, and the release's own
//! directory. It is a containment device — the writes and the processes are what
//! is being contained.
//!
//! [bubblewrap]: https://github.com/containers/bubblewrap

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use crate::wine::{has_tool, is_yes};

/// Entry key: run this demo in a sandboxed, throwaway copy of the prefix.
///
/// On by default wherever it works, which is what makes a grid of Windows demos
/// possible at all. `wine_sandbox=false` puts the session back in the real
/// `~/.wine-demarc` — for provisioning a release that wants to *keep* what it
/// installs, and for the machine where `bwrap` cannot run.
pub const META_SANDBOX: &str = "wine_sandbox";

/// See [`META_SANDBOX`].
pub const DEFAULT_SANDBOX: bool = true;

const BWRAP: &str = "bwrap";

/// Where the mount points live, under `$XDG_RUNTIME_DIR` when there is one.
///
/// They are only ever empty directories on this side — the overlay exists in
/// the sandbox's mount namespace and nowhere else — so a tmpfs is exactly the
/// right place for them.
const SANDBOX_DIR: &str = "demarc-wine";

/// Distinguishes one session from the next within a run of demarc; the pid
/// distinguishes one run from the next.
static SEQ: AtomicU32 = AtomicU32::new(0);

/// A prepared sandbox: where the prefix will appear, and the `bwrap` invocation
/// that puts it there.
pub struct Sandbox {
    /// `WINEPREFIX` for the session. Inside the sandbox it is the overlay; out
    /// here it is an empty directory, which is all anyone else can see of it.
    pub prefix: PathBuf,
    /// `bwrap` and its arguments, ending in `--`. [`Sandbox::wrap`] is the way
    /// to use it.
    argv: Vec<String>,
}

impl Sandbox {
    /// The command to run, with the sandbox in front of it.
    pub fn wrap(&self, command: Vec<String>) -> Vec<String> {
        let mut argv = self.argv.clone();
        argv.extend(command);
        argv
    }
}

/// The uid, which names both the socket directory wine uses and the fallback
/// sandbox directory under `/tmp`.
fn uid() -> u32 {
    // SAFETY: `getuid` reads no memory of ours and cannot fail.
    unsafe { libc::getuid() }
}

/// Where this run keeps its mount points: `<runtime>/demarc-wine-<uid>/<pid>`.
///
/// Split by pid so [`sweep`] can throw away a dead run's directories whole
/// without having to decide anything about the live ones.
fn run_dir() -> PathBuf {
    let root = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join(format!("{SANDBOX_DIR}-{}", uid()))
        .join(std::process::id().to_string())
}

/// Throw away the mount points of demarcs that are no longer running.
///
/// A crash leaves them behind — they are empty directories, so this is tidiness
/// rather than repair, but an unbounded pile of them under `$XDG_RUNTIME_DIR` is
/// still a pile. A directory named after a live pid is left alone; that is
/// either us or another demarc with sessions in it.
fn sweep(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if Path::new(&format!("/proc/{pid}")).exists() {
            continue;
        }
        debug!("Removing wine sandbox mount points left by pid {pid}");
        let _ = fs::remove_dir_all(entry.path());
    }
}

/// The `bwrap` arguments that put `base` at `prefix` as a throwaway overlay.
///
/// Ends in `--`; the command goes after it. Nothing is quoted, because no shell
/// is involved.
fn bwrap_args(base: &Path, prefix: &Path, workdir: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = vec![
        BWRAP.into(),
        // The host as it stands, devices included. The demo needs the GPU
        // nodes, the audio socket, gamescope's X socket and its own directory,
        // and there is no shorter list that is still correct for every release.
        "--dev-bind".into(),
        "/".into(),
        "/".into(),
        // Must come after the bind, which brought the host's own /proc with it.
        // A pid namespace with the wrong /proc in it is worse than none.
        "--proc".into(),
        "/proc".into(),
        // What ends the session cleanly: wine's services cannot setsid their
        // way out of a pid namespace, so the last of them goes when pid 1 does.
        "--unshare-pid".into(),
        "--die-with-parent".into(),
        // wine keeps its server socket in /tmp/.wine-<uid> and names it after
        // the prefix's device and inode. A private tmpfs there is what makes the
        // server private for certain rather than by coincidence — and 0700,
        // because wineserver refuses a socket directory others can read.
        "--perms".into(),
        "0700".into(),
        "--tmpfs".into(),
        format!("/tmp/.wine-{}", uid()),
        // The prefix itself: the real one underneath, an invisible tmpfs on top.
        "--overlay-src".into(),
        base.to_string_lossy().into_owned(),
        "--tmp-overlay".into(),
        prefix.to_string_lossy().into_owned(),
        // Said here as well as by whoever spawns us, so the sandbox describes
        // itself and the two cannot disagree about which prefix this is.
        "--setenv".into(),
        "WINEPREFIX".into(),
        prefix.to_string_lossy().into_owned(),
    ];
    // A release that ships a `data/` folder or its own `fmod.dll` finds neither
    // from anywhere else. bwrap keeps the working directory when it can, but
    // saying it outright costs nothing and survives being spawned from a core
    // that chose its own.
    // Absolute, because `bwrap` chdirs inside the new mount namespace, where a
    // relative path means nothing and the sandbox refuses to start at all.
    if let Some(dir) = workdir.map(|dir| dir.canonicalize().unwrap_or(dir.to_owned())) {
        args.extend(["--chdir".into(), dir.to_string_lossy().into_owned()]);
    }
    args.push("--".into());
    args
}

/// Can this machine actually run the sandbox? Asked once, and answered by
/// trying it.
///
/// Neither `bwrap` being installed nor the kernel having unprivileged user
/// namespaces is a safe assumption — Ubuntu's AppArmor turns the second off by
/// default, and an overlay inside a user namespace is newer still. Guessing
/// from versions would be worse than the real thing, which is one process spawn
/// on the first Windows demo of a run: the whole argument list, with `true` in
/// place of the demo.
fn usable(base: &Path) -> bool {
    static USABLE: OnceLock<bool> = OnceLock::new();
    *USABLE.get_or_init(|| {
        if !has_tool(BWRAP) {
            info!("No `{BWRAP}` on PATH; Windows demos share one wine prefix");
            return false;
        }
        // Overlaid onto itself: the probe wants a real overlay of a real
        // prefix, and inside the sandbox that is all this is. Nothing is
        // written, and the mount is gone with the process.
        let args = bwrap_args(base, base, None);
        let ok = Command::new(&args[0])
            .args(&args[1..])
            .arg("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if !ok {
            warn!(
                "`{BWRAP}` cannot make a sandbox here (unprivileged user namespaces or \
                 overlayfs unavailable); Windows demos share one wine prefix, one at a time"
            );
        }
        ok
    })
}

/// Does this entry want a sandbox? See [`META_SANDBOX`].
pub fn wanted(meta: &HashMap<String, String>) -> bool {
    meta.get(META_SANDBOX)
        .map(|v| is_yes(v))
        .unwrap_or(DEFAULT_SANDBOX)
}

/// Prepare a sandbox around `base`, with `workdir` as the working directory.
///
/// Fails rather than falling back, so the caller decides what running without
/// one means for it — which is not the same answer in both backends.
pub fn prepare(base: &Path, workdir: Option<&Path>) -> Result<Sandbox> {
    if !base.is_dir() {
        // Nothing to overlay: a first run, before wine has built the prefix.
        // Sandboxing it would build a prefix inside a tmpfs and throw it away
        // again, paying `wineboot` every time and keeping nothing, so the first
        // session runs unsandboxed and creates it for all the rest.
        bail!("no wine prefix at {}", base.display());
    }
    if !usable(base) {
        bail!("`{BWRAP}` cannot make a sandbox here");
    }

    let run = run_dir();
    if let Some(parent) = run.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Could not make {}", parent.display()))?;
        sweep(parent);
    }
    fs::create_dir_all(&run).with_context(|| format!("Could not make {}", run.display()))?;

    let prefix = run.join(SEQ.fetch_add(1, Ordering::Relaxed).to_string());
    fs::create_dir(&prefix)
        .with_context(|| format!("Could not make the mount point {}", prefix.display()))?;

    debug!(
        "Sandboxing the wine prefix {} as {}",
        base.display(),
        prefix.display()
    );
    Ok(Sandbox {
        argv: bwrap_args(base, &prefix, workdir),
        prefix,
    })
}

#[cfg(test)]
#[path = "tests/wine_sandbox_tests.rs"]
mod tests;
