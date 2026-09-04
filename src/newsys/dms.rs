//! Turning a DMS archive back into the floppy image inside it.
//!
//! DMS (Disk Masher System) is how a great deal of the Amiga scene was
//! distributed: a whole floppy, track by track, in one compressed file. Both
//! Amiga cores read `.dms` directly, so nothing here is needed to *boot* one —
//! it exists for `--unadf` (see [`AmigaSystem::load`](super::amiga)), whose
//! ADFlib walk only understands a plain sector image. Unpacking to an `.adf`
//! first is what lets a DMS release take the faster hard-drive path too.
//!
//! The unpacker is xDMS 1.3 by Andre Rodrigues de la Rocha (public domain),
//! taken from amiberry's copy and kept in `external/dms`; the C entry point is
//! `src/dms_unpack_shim.c`. The one thing worth knowing about it from here is
//! that it writes each track at its own offset in the output, so a truncated
//! or partly corrupt archive still yields an image with whatever tracks it did
//! contain in the right places — including, usefully, the boot block and the
//! root directory.
//!
//! xDMS keeps the decrunchers' state — the sliding window, the bit buffer, the
//! Huffman tables — in globals, so only one unpack may be in flight at a time
//! (see [`XDMS`]). Releases are loaded on a worker thread (see `crate::jobs`),
//! and with `--grid` several of them are loaded at once, so this is a real race
//! and not a theoretical one: running two of these at once corrupts the heap.

use std::ffi::CString;
use std::os::raw::{c_char, c_int};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Result, bail};

unsafe extern "C" {
    /// Unpacks the archive at `dms_path` into a newly created file at
    /// `adf_path`. Returns the size of the image written, or a negative
    /// `DMS_ERR_*`.
    fn demarc_dms_unpack(dms_path: *const c_char, adf_path: *const c_char) -> c_int;
}

/// The archive could not be opened for reading.
const DMS_ERR_OPEN: c_int = -1;
/// The output could not be created or written.
const DMS_ERR_WRITE: c_int = -2;
/// Not a DMS archive (or an FMS one, which holds files rather than a disk).
const DMS_ERR_NOTDMS: c_int = -3;
/// A DMS archive that would not come apart: a bad CRC, an unknown compression
/// mode, or — the one we cannot do anything about — a password on it.
const DMS_ERR_UNPACK: c_int = -4;
/// It unpacked into nothing at all.
const DMS_ERR_EMPTY: c_int = -5;

/// xDMS is a single-threaded program that happens to have a function in it:
/// `dms_text` (the LZ window), the bit buffer in `getbits.c` and every
/// decruncher's tables are file-scope globals shared by whoever calls in.
static XDMS: Mutex<()> = Mutex::new(());

/// Is `path` named like a DMS archive?
pub fn is_dms(path: &Path) -> bool {
    crate::utils::has_extension(path, "dms")
}

/// Unpack the archive `archive` into a new floppy image at `image`, which is
/// created, or truncated if it is already there.
///
/// Returns the size of the image written — 901,120 bytes for the ordinary
/// double-density disk, twice that for an HD one, and less than either when the
/// archive was missing tracks.
pub fn unpack(archive: &Path, image: &Path) -> Result<u64> {
    // The C side takes paths as C strings, so a path that isn't UTF-8 (or holds
    // an interior NUL) can't be passed on. Rare enough to just decline.
    let (Some(archive_str), Some(image_str)) = (archive.to_str(), image.to_str()) else {
        bail!("Path is not valid UTF-8: {archive:?}");
    };
    let archive_c = CString::new(archive_str)?;
    let image_c = CString::new(image_str)?;

    // Poisoning would mean an earlier unpack panicked; the globals are reset at
    // the start of every call, so carry on.
    let _guard = XDMS.lock().unwrap_or_else(|e| e.into_inner());

    // Safety: both pointers are valid, NUL-terminated C strings that outlive
    // the call, the shim only reads them, and the lock above keeps xDMS's
    // globals to one caller at a time.
    let size = unsafe { demarc_dms_unpack(archive_c.as_ptr(), image_c.as_ptr()) };

    match size {
        DMS_ERR_OPEN => bail!("Could not read {archive:?}"),
        DMS_ERR_WRITE => bail!("Could not write {image:?}"),
        DMS_ERR_NOTDMS => bail!("Not a DMS archive: {archive:?}"),
        DMS_ERR_UNPACK => bail!("Could not unpack {archive:?} (bad data, or encrypted)"),
        DMS_ERR_EMPTY => bail!("No disk in {archive:?}"),
        n if n < 0 => bail!("Could not unpack {archive:?} ({n})"),
        n => Ok(n as u64),
    }
}

#[cfg(test)]
#[path = "tests/dms_tests.rs"]
mod tests;
