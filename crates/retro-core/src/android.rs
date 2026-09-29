//! The Android side of the player: logging and the `android_main` entry point.
//!
//! Android gives a native app no stdout — `println!` and the default
//! `tracing_subscriber` writer both go nowhere — so everything, including a
//! panic, has to be handed to `liblog` instead. See `docs/ANDROID.md`.

use std::ffi::CString;
use std::io;

use tracing::Level;
use tracing::metadata::Metadata;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

const TAG: &str = "demarc";

// liblog is already linked in through android-activity's ndk dependency; this
// only names the symbol.
unsafe extern "C" {
    fn __android_log_write(
        prio: i32,
        tag: *const std::ffi::c_char,
        text: *const std::ffi::c_char,
    ) -> i32;
}

fn priority(level: Level) -> i32 {
    match level {
        Level::TRACE => 2,
        Level::DEBUG => 3,
        Level::INFO => 4,
        Level::WARN => 5,
        Level::ERROR => 6,
    }
}

fn write_line(prio: i32, line: &str) {
    let tag = CString::new(TAG).unwrap();
    // A NUL in the message would truncate it; a core's log line can hold one.
    let text = CString::new(line.replace('\0', "?")).unwrap();
    unsafe { __android_log_write(prio, tag.as_ptr(), text.as_ptr()) };
}

/// Collects one formatted event and emits it as logcat lines when dropped.
struct LogcatWriter {
    prio: i32,
    buf: Vec<u8>,
}

impl io::Write for LogcatWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        let text = String::from_utf8_lossy(&self.buf);
        for line in text.lines().filter(|l| !l.is_empty()) {
            write_line(self.prio, line);
        }
        self.buf.clear();
        Ok(())
    }
}

impl Drop for LogcatWriter {
    fn drop(&mut self) {
        _ = io::Write::flush(self);
    }
}

struct Logcat;

impl<'a> MakeWriter<'a> for Logcat {
    type Writer = LogcatWriter;

    fn make_writer(&'a self) -> Self::Writer {
        LogcatWriter {
            prio: priority(Level::INFO),
            buf: Vec::new(),
        }
    }

    fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Self::Writer {
        LogcatWriter {
            prio: priority(*meta.level()),
            buf: Vec::new(),
        }
    }
}

/// Send `tracing` and panics to logcat: `adb logcat -s demarc:V`.
pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_writer(Logcat)
        .with_ansi(false)
        .without_time()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_line(priority(Level::ERROR), &format!("panic: {info}"));
        previous(info);
    }));
}
