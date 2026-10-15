//! wine's and Mesa's stderr, read off a FIFO into the log.
//!
//! Goes to the `retro` target like a core's own log: what names a failure as a
//! warning, the rest at debug. A line is only logged once.

use std::collections::HashSet;
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

static SEQ: AtomicU32 = AtomicU32::new(0);

const POLL: Duration = Duration::from_millis(50);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_DISTINCT: usize = 4096;

const SCRIPT: &str = r#"export WINEDEBUG="$1" MESA_DEBUG="${MESA_DEBUG-1}" MESA_GLSL="${MESA_GLSL-dump_on_error}"; shift; exec "$@" 2>"$0""#;

/// The core's own default is `-all`, which hides a missing DLL too.
const WINEDEBUG: &str = "fixme-all";

const ERRORS: &[&str] = &[
    "wine: ",
    "err:module:",
    "err:seh:",
    "unimplemented function",
    "Mesa: error",
    "error: ",
    "GLSL",
];

/// `argv` with its stderr sent to the log. Unchanged if the FIFO can't be made.
pub fn capture(argv: Vec<String>) -> Vec<String> {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let fifo = std::env::temp_dir().join(format!("demarc-wine-{}-{seq}.log", std::process::id()));
    let (Ok(name), Some(arg)) = (CString::new(fifo.as_os_str().as_bytes()), fifo.to_str()) else {
        return argv;
    };
    // SAFETY: `name` is a valid C string for the duration of the call.
    if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
        return argv;
    }
    let pipe = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo);
    let Ok(pipe) = pipe else {
        let _ = fs::remove_file(&fifo);
        return argv;
    };
    let winedebug = std::env::var("WINEDEBUG").unwrap_or_else(|_| WINEDEBUG.to_string());
    let mut wrapped = vec![
        "sh".to_string(),
        "-c".to_string(),
        SCRIPT.to_string(),
        arg.to_string(),
        winedebug,
    ];
    wrapped.extend(argv);
    thread::spawn(move || read_log(pipe, &fifo));
    wrapped
}

fn is_error(line: &str) -> bool {
    ERRORS.iter().any(|pattern| line.contains(pattern))
}

/// The FIFO's name goes as soon as the session has it open.
fn read_log(mut pipe: File, fifo: &Path) {
    let started = Instant::now();
    let mut opened = false;
    let mut seen = HashSet::new();
    let mut pending = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf) {
            // No writer: not started yet, or all of them gone.
            Ok(0) if opened || started.elapsed() > START_TIMEOUT => break,
            Ok(0) => thread::sleep(POLL),
            Ok(n) => {
                if !opened {
                    let _ = fs::remove_file(fifo);
                }
                opened = true;
                pending.extend_from_slice(&buf[..n]);
                while let Some(end) = pending.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = pending.drain(..=end).collect();
                    log_line(String::from_utf8_lossy(&line).trim_end(), &mut seen);
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                if !opened {
                    let _ = fs::remove_file(fifo);
                }
                opened = true;
                thread::sleep(POLL);
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    if !opened {
        let _ = fs::remove_file(fifo);
    }
}

fn log_line(line: &str, seen: &mut HashSet<String>) {
    if line.is_empty() || seen.len() >= MAX_DISTINCT || !seen.insert(line.to_string()) {
        return;
    }
    if is_error(line) {
        warn!(target: "retro", "{line}");
    } else {
        debug!(target: "retro", "{line}");
    }
}

#[cfg(test)]
#[path = "tests/wine_log_tests.rs"]
mod tests;
