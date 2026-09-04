//! Unpacking an AmigaDOS floppy image (`.adf`) into a real directory.
//!
//! This backs `--unadf` (see [`AmigaSystem::load`](super::amiga)): a demo that
//! ships as a bootable AmigaDOS disk can be handed to the emulator as a hard
//! drive instead, which skips the floppy seeks and the disk-change waits and so
//! reaches the first frame a good deal faster.
//!
//! It only works for a disk with a real file system on it. Plenty of demo disks
//! are trackloaded — a custom boot block and raw sectors, no AmigaDOS structures
//! at all — and those mount as nothing and have to stay floppies. Being handed
//! one of those is an expected outcome, not a fault: it comes back as an `Err`
//! that the caller logs and shrugs off, and the disk is booted as a floppy.
//!
//! The work is done by ADFlib (`external/ADFlib`) through the C shim in
//! `src/adf_unpack_shim.c`, which is where the reasoning about why the walk is
//! C rather than Rust lives.
//!
//! Not the `adflib` crate on crates.io, which was tried first and cannot do
//! this: as of 0.1.7 (its newest release) it scans only longwords 24..51 of a
//! directory's 72-entry hash table and never follows the `next_hash` chains, so
//! it silently misses most of the files; it reads an entry's size out of the
//! `header_key` field, so the sizes it does report are block numbers; and
//! `FileInfo` carries no block pointer while `extract_file` refuses directories
//! and only searches the root, so there is no way to descend into `s/` at all.
//! On `Ghostown-SushiBoyzParty.adf` it finds 2 of the 6 root entries, reports
//! `S/` as a file, and never sees the demo executable. This code's output is
//! byte-for-byte identical to ADFlib's own `unadf` on that disk — the one place
//! the two part company is a file name outside ASCII, which `unadf` writes as
//! the Latin-1 the disk holds and this writes as the UTF-8 the cores read (see
//! `safe_name` in the shim).

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;
use std::sync::{Mutex, Once};

use anyhow::{Result, bail};

unsafe extern "C" {
    /// Registers ADFlib's built-in device drivers. Must be called once, and
    /// only once, before [`demarc_adf_unpack`] — see [`init`].
    fn demarc_adf_init();

    /// Unpacks the image at `adf_path` into the existing directory `dest_dir`.
    /// Returns the number of entries extracted, or a negative `ADF_ERR_*`.
    fn demarc_adf_unpack(adf_path: *const c_char, dest_dir: *const c_char) -> c_int;
}

/// The image is not something ADFlib can open at all.
const ADF_ERR_OPEN: c_int = -1;
/// No AmigaDOS volume on it — a trackloaded demo disk, most likely.
const ADF_ERR_MOUNT: c_int = -2;
/// The host side of the copy failed (out of space, unwritable directory).
const ADF_ERR_IO: c_int = -3;

/// ADFlib keeps its environment — the log callbacks, the device driver list,
/// the dir-cache flag — in globals, so only one unpack may be in flight at a
/// time. Releases are loaded on a worker thread (see `crate::jobs`), and with
/// `--grid` several of them are loaded at once, so this is a real race and not
/// a theoretical one.
static ADFLIB: Mutex<()> = Mutex::new(());

/// `adfAddDeviceDriver` appends to a global list with no duplicate check, so
/// initialising per unpack would grow that list without bound.
static INIT: Once = Once::new();

fn init() {
    // Safety: `Once` guarantees exactly one call, and `demarc_adf_init` only
    // writes ADFlib's globals, which nothing else has touched yet.
    INIT.call_once(|| unsafe { demarc_adf_init() });
}

/// Unpack the floppy image `image` into the existing directory `dest`.
///
/// Returns the number of files and directories written, which is zero for a
/// volume that mounted but held nothing. Errors describe a disk that could not
/// be read as AmigaDOS; the caller is expected to treat that as "boot it as a
/// floppy after all" rather than as a failure to load the release.
pub fn unpack(image: &Path, dest: &Path) -> Result<usize> {
    // ADFlib takes paths as C strings, so a path that isn't UTF-8 (or holds an
    // interior NUL) can't be passed on. Rare enough to just decline.
    let (Some(image_str), Some(dest_str)) = (image.to_str(), dest.to_str()) else {
        bail!("Path is not valid UTF-8: {image:?}");
    };
    let image_c = CString::new(image_str)?;
    let dest_c = CString::new(dest_str)?;

    init();
    // Poisoning would mean an earlier unpack panicked; ADFlib's globals are
    // re-initialised per call apart from the driver list, so carry on.
    let _guard = ADFLIB.lock().unwrap_or_else(|e| e.into_inner());

    // Safety: both pointers are valid, NUL-terminated C strings that outlive
    // the call, the shim only reads them, and the lock above keeps ADFlib's
    // globals to one caller at a time.
    let count = unsafe { demarc_adf_unpack(image_c.as_ptr(), dest_c.as_ptr()) };

    match count {
        ADF_ERR_OPEN => bail!("Not an ADF image: {image:?}"),
        ADF_ERR_MOUNT => bail!("No AmigaDOS file system on {image:?}"),
        ADF_ERR_IO => bail!("Failed writing the contents of {image:?} to {dest:?}"),
        n if n < 0 => bail!("Could not unpack {image:?} ({n})"),
        n => Ok(n as usize),
    }
}

#[cfg(test)]
#[path = "tests/adf_tests.rs"]
mod tests;

