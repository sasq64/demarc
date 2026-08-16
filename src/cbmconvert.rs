//! Thin FFI wrapper around the `cbmconvert` command-line tool.
//!
//! The C sources under `cbmconvert/` are compiled into the binary by
//! `build.rs`, with `main` renamed to `cbmconvert_main` (via `-Dmain=...`) so we
//! can call it directly instead of spawning an external executable.
//!
//! Note: `cbmconvert_main` relies on file-scope state and writes output files
//! relative to the current working directory, exactly as the CLI does. It is a
//! one-shot entry point — treat it as a single conversion invocation per call,
//! not a reentrant library.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

unsafe extern "C" {
    /// The renamed `main()` of cbmconvert. Same contract as the CLI: `argv[0]`
    /// is the program name, the remaining entries are the arguments, and the
    /// return value is the process exit status (0 == success).
    fn cbmconvert_main(argc: c_int, argv: *const *const c_char) -> c_int;
}

/// Run cbmconvert with `args` (the flags/files you would pass on the command
/// line, excluding the program name). Output files are written relative to the
/// current working directory, just like the CLI.
///
/// Returns cbmconvert's exit code: 0 on success, non-zero on error.
///
/// # Panics
/// Panics if any argument contains an interior NUL byte.
pub fn run<I, S>(args: I) -> i32
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // argv[0] is the program name; the C code uses it only for diagnostics.
    let mut owned: Vec<CString> = vec![CString::new("cbmconvert").unwrap()];
    for arg in args {
        owned.push(CString::new(arg.as_ref()).expect("argument contains a NUL byte"));
    }

    // Build a NULL-terminated argv array of borrowed pointers into `owned`.
    let mut argv: Vec<*const c_char> = owned.iter().map(|s| s.as_ptr()).collect();
    argv.push(std::ptr::null());

    // Safety: `argv` is a valid, NULL-terminated array of `argc` C strings that
    // outlive the call, and cbmconvert only reads (never mutates) them.
    unsafe { cbmconvert_main(owned.len() as c_int, argv.as_ptr()) }
}

/// Serializes the working-directory switch below. The CWD is process-wide, so
/// two conversions running at once would write their output into each other's
/// directory. Holding the lock also keeps [`run`]'s file-scope state to one
/// conversion at a time.
static CWD_LOCK: Mutex<()> = Mutex::new(());

/// Switches the process-wide working directory for the duration of a
/// conversion — the only way to tell cbmconvert where to put its output — and
/// restores the previous one on drop.
pub struct CwdGuard {
    prev: PathBuf,
    // Dropped after `prev` is restored, so the next conversion starts from a
    // known directory.
    _lock: MutexGuard<'static, ()>,
}
impl CwdGuard {
    pub fn enter(dir: &Path) -> Self {
        // A conversion that panicked left the CWD restored by this same guard,
        // so the poisoned state is fine to keep using.
        let lock = CWD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        CwdGuard { prev, _lock: lock }
    }
}
impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Convert `testdata/c64/BADALM.T64` (a C64 tape image) to a raw `.prg` file
    /// and verify cbmconvert produced it with the expected `$0801` load address.
    #[test]
    fn t64_to_prg() {
        let t64 = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/c64/BADALM.T64");
        assert!(t64.is_file(), "missing test fixture: {}", t64.display());

        // cbmconvert writes output relative to the CWD, so run inside a temp dir.
        let dir = tempfile::tempdir().unwrap();

        // -t: read T64 input, -N: write native (raw .prg with load address).
        let code = {
            let _guard = CwdGuard::enter(dir.path());
            run(["-t", "-N", t64.to_str().unwrap()])
        };
        assert_eq!(code, 0, "cbmconvert exited with {code}");

        // The T64 entry is named "BADALM"; native output lowercases it.
        let prg = dir.path().join("badalm.prg");
        assert!(prg.is_file(), "expected {} to be created", prg.display());

        let bytes = std::fs::read(&prg).unwrap();
        assert!(bytes.len() > 2, "prg is unexpectedly small");
        // Native .prg starts with a little-endian load address; BADALM is $0801.
        assert_eq!(&bytes[..2], &[0x01, 0x08], "unexpected load address");
    }
}
