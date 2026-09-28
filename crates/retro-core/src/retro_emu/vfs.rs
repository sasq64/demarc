//! The frontend file system interface (`RETRO_ENVIRONMENT_GET_VFS_INTERFACE`).
//!
//! Cores that want to touch a file are supposed to ask the frontend to do it,
//! rather than calling `fopen` themselves — that is how RetroArch supports
//! Android SAF URIs, network shares and paths inside archives, none of which a
//! plain `open()` can resolve. For demarc, every path *is* an ordinary
//! filesystem path, so this is a thin wrapper over `std::fs`.
//!
//! It is not optional any more. Upstream Stella
//! (`stella-emu/stella` 51994c0, 2026-05-17, "libretro: Make FSNodeLIBRETRO a
//! proper FSNode implementation") made its `FSNode::isFile()` default to
//! `false` and set it *only* from the VFS `stat()`. With no VFS on offer,
//! `libretro_vfs` stays null, the flag stays false, and `OSystem::openROM`
//! throws "Unrecognized ROM file type" for every ROM — the check runs before
//! anything looks at the image, so nothing at all loads. Worse, without a VFS
//! Stella falls back to the in-memory image padded out to `Cartridge::maxSize()`,
//! so it hashes 512K of mostly zeroes: a 32K demo came out as MD5
//! 16cf3ddf… "4K* (512K)" instead of 9c0e06f1… "F4* (32K)", i.e. wrong
//! bankswitch type, wrong per-ROM properties, wrong TV format. Answering this
//! call fixes the load *and* the detection. Expect more cores to follow.
//!
//! We advertise v3 (the version that added `stat`/`mkdir` and the directory
//! calls); `stat_64` from v4 is not in our bindings and no core we load needs it.
//!
//! Handles are `Box::into_raw`'d Rust structs handed back to the core as the
//! opaque `retro_vfs_file_handle`/`retro_vfs_dir_handle`. A core owns each
//! handle between `open` and `close` and never shares one across threads, so
//! the `&mut` we take on every call cannot alias.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::libretro::{
    RETRO_VFS_FILE_ACCESS_READ, RETRO_VFS_FILE_ACCESS_UPDATE_EXISTING, RETRO_VFS_FILE_ACCESS_WRITE,
    RETRO_VFS_SEEK_POSITION_CURRENT, RETRO_VFS_SEEK_POSITION_END, RETRO_VFS_SEEK_POSITION_START,
    RETRO_VFS_STAT_IS_DIRECTORY, RETRO_VFS_STAT_IS_VALID, retro_vfs_dir_handle,
    retro_vfs_file_handle, retro_vfs_interface,
};
// Only Unix has a file type to report here; on Windows the flag never applies.
#[cfg(unix)]
use crate::libretro::RETRO_VFS_STAT_IS_CHARACTER_SPECIAL;

/// Highest VFS API version we implement. A core asking for more is refused, as
/// the API requires, and falls back to whatever it does without a VFS.
pub const VERSION: u32 = 3;

/// Returns the shared interface, building it once.
///
/// The API says the interface is owned by the frontend and must outlive every
/// core, so it is leaked deliberately: one 152-byte table of function pointers
/// for the life of the process, shared by all cores. Nothing in it is per-core
/// state, so there is nothing to tear down.
pub fn interface() -> *mut retro_vfs_interface {
    static IFACE: OnceLock<usize> = OnceLock::new();
    *IFACE.get_or_init(|| {
        Box::into_raw(Box::new(retro_vfs_interface {
            get_path: Some(get_path),
            open: Some(open),
            close: Some(close),
            size: Some(size),
            tell: Some(tell),
            seek: Some(seek),
            read: Some(read),
            write: Some(write),
            flush: Some(flush),
            remove: Some(remove),
            rename: Some(rename),
            truncate: Some(truncate),
            stat: Some(stat),
            mkdir: Some(mkdir),
            opendir: Some(opendir),
            readdir: Some(readdir),
            dirent_get_name: Some(dirent_get_name),
            dirent_is_dir: Some(dirent_is_dir),
            closedir: Some(closedir),
        })) as usize
    }) as *mut retro_vfs_interface
}

/// Converts a C path from the core into a `PathBuf`.
///
/// On Unix the bytes are the path — no UTF-8 round trip, so a filename that
/// isn't valid UTF-8 (which demo archives do produce) still opens. Elsewhere
/// we require UTF-8, which is what those platforms hand us anyway.
unsafe fn to_path(path: *const c_char) -> Option<PathBuf> {
    if path.is_null() {
        return None;
    }
    let bytes = unsafe { CStr::from_ptr(path) };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes.to_bytes())))
    }
    #[cfg(not(unix))]
    {
        bytes.to_str().ok().map(PathBuf::from)
    }
}

// - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -
// Files

struct FileHandle {
    file: File,
    /// Returned verbatim by `get_path`, so it has to stay put for as long as
    /// the handle lives — hence owned here rather than rebuilt per call.
    path: CString,
}

/// Borrows a handle the core gave back to us. `None` for a null pointer, which
/// a core should never pass but the API lets it.
unsafe fn file<'a>(stream: *mut retro_vfs_file_handle) -> Option<&'a mut FileHandle> {
    (!stream.is_null()).then(|| unsafe { &mut *(stream as *mut FileHandle) })
}

unsafe extern "C" fn get_path(stream: *mut retro_vfs_file_handle) -> *const c_char {
    match unsafe { file(stream) } {
        Some(h) => h.path.as_ptr(),
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn open(
    path: *const c_char,
    mode: c_uint,
    _hints: c_uint,
) -> *mut retro_vfs_file_handle {
    let Some(p) = (unsafe { to_path(path) }) else {
        return std::ptr::null_mut();
    };
    // `get_path` must hand back exactly the string we were given, so keep the
    // original bytes instead of re-encoding the PathBuf.
    let stored = unsafe { CStr::from_ptr(path) }.to_owned();

    let read = mode & RETRO_VFS_FILE_ACCESS_READ != 0;
    let write = mode & RETRO_VFS_FILE_ACCESS_WRITE != 0;
    // "Opens a file without discarding its existing contents. Only meaningful
    // if WRITE is specified." Without it, a write mode truncates.
    let keep = mode & RETRO_VFS_FILE_ACCESS_UPDATE_EXISTING != 0;

    let mut opts = OpenOptions::new();
    opts.read(read);
    if write {
        opts.write(true);
        if keep {
            // Update in place: the file must already exist, and its contents
            // survive. This is the mode used to patch a save file.
            opts.create(false).truncate(false);
        } else {
            opts.create(true).truncate(true);
        }
    } else if !read {
        // Neither READ nor WRITE: the API says at least one is required.
        return std::ptr::null_mut();
    }

    // `opendir` is the call for directories; open() must fail on one. Without
    // this an O_RDONLY open of a directory succeeds on Linux and only fails
    // later, at the first read.
    if p.is_dir() {
        return std::ptr::null_mut();
    }

    match opts.open(&p) {
        Ok(file) => {
            Box::into_raw(Box::new(FileHandle { file, path: stored })) as *mut retro_vfs_file_handle
        }
        Err(_) => std::ptr::null_mut(),
    }
}

unsafe extern "C" fn close(stream: *mut retro_vfs_file_handle) -> c_int {
    if stream.is_null() {
        return -1;
    }
    // Dropping the Box closes the file.
    drop(unsafe { Box::from_raw(stream as *mut FileHandle) });
    0
}

unsafe extern "C" fn size(stream: *mut retro_vfs_file_handle) -> i64 {
    unsafe { file(stream) }
        .and_then(|h| h.file.metadata().ok())
        .map_or(-1, |m| m.len() as i64)
}

unsafe extern "C" fn truncate(stream: *mut retro_vfs_file_handle, length: i64) -> i64 {
    let Some(h) = (unsafe { file(stream) }) else {
        return -1;
    };
    if length < 0 {
        return -1;
    }
    match h.file.set_len(length as u64) {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn tell(stream: *mut retro_vfs_file_handle) -> i64 {
    unsafe { file(stream) }
        .and_then(|h| h.file.stream_position().ok())
        .map_or(-1, |p| p as i64)
}

/// Seeks, returning **0 on success** — `fseek` semantics, not the new position.
///
/// Read that twice, because the header says otherwise: `retro_vfs_seek_t` is
/// documented as "The new position, or -1 if there was an error". Nothing
/// implements that. libretro-common's reference `retro_vfs_file_seek_impl`
/// ends in a plain `fseeko(...)` and so returns 0/-1, which is what RetroArch
/// hands cores, and cores are written against it: a core that includes
/// `file_stream_transforms.h` gets `#define fseek rfseek`, so its ordinary
/// `if (fseek(f, off, SEEK_SET)) return -1;` treats any non-zero return as
/// failure.
///
/// Returning the position instead looks fine until an offset is non-zero.
/// pcsx_rearmed reading sector 0 of a disc works (offset 0); sector 4 seeks to
/// 9408, the core reads that as an error and gives up with "cdrom read failed
/// for lba 4: -1", and the whole disc is rejected as an "unsupported/invalid CD
/// image". Every CD-based system in demarc failed this way.
unsafe extern "C" fn seek(
    stream: *mut retro_vfs_file_handle,
    offset: i64,
    seek_position: c_int,
) -> i64 {
    let Some(h) = (unsafe { file(stream) }) else {
        return -1;
    };
    let from = match seek_position as u32 {
        RETRO_VFS_SEEK_POSITION_START => SeekFrom::Start(offset.max(0) as u64),
        RETRO_VFS_SEEK_POSITION_CURRENT => SeekFrom::Current(offset),
        RETRO_VFS_SEEK_POSITION_END => SeekFrom::End(offset),
        _ => return -1,
    };
    h.file.seek(from).map_or(-1, |_| 0)
}

unsafe extern "C" fn read(stream: *mut retro_vfs_file_handle, s: *mut c_void, len: u64) -> i64 {
    let Some(h) = (unsafe { file(stream) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts_mut(s as *mut u8, len as usize) };
    // A short read is not an error here: the caller gets the count and asks
    // again, which is how `filestream_read` behaves.
    h.file.read(buf).map_or(-1, |n| n as i64)
}

unsafe extern "C" fn write(stream: *mut retro_vfs_file_handle, s: *const c_void, len: u64) -> i64 {
    let Some(h) = (unsafe { file(stream) }) else {
        return -1;
    };
    if s.is_null() {
        return -1;
    }
    if len == 0 {
        return 0;
    }
    let buf = unsafe { std::slice::from_raw_parts(s as *const u8, len as usize) };
    h.file.write(buf).map_or(-1, |n| n as i64)
}

unsafe extern "C" fn flush(stream: *mut retro_vfs_file_handle) -> c_int {
    match unsafe { file(stream) } {
        Some(h) => h.file.flush().map_or(-1, |()| 0),
        None => -1,
    }
}

// - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -
// Paths

unsafe extern "C" fn remove(path: *const c_char) -> c_int {
    match unsafe { to_path(path) } {
        Some(p) => std::fs::remove_file(p).map_or(-1, |()| 0),
        None => -1,
    }
}

unsafe extern "C" fn rename(old_path: *const c_char, new_path: *const c_char) -> c_int {
    let (Some(old), Some(new)) = (unsafe { to_path(old_path) }, unsafe { to_path(new_path) })
    else {
        return -1;
    };
    std::fs::rename(old, new).map_or(-1, |()| 0)
}

unsafe extern "C" fn stat(path: *const c_char, size: *mut i32) -> c_int {
    let Some(p) = (unsafe { to_path(path) }) else {
        return 0;
    };
    // Deliberately `metadata`, not `symlink_metadata`: a symlink to a ROM
    // should stat as the ROM.
    let Ok(meta) = std::fs::metadata(&p) else {
        return 0; // not a valid path — the API's way of saying "no such file"
    };

    if !size.is_null() {
        // The out-parameter is i32, so anything past 2GB can't be reported
        // faithfully. Saturate rather than wrap: a caller that size-checks
        // against a cartridge limit then rejects the file instead of seeing a
        // negative or absurdly small one. Nothing demarc loads is that big.
        unsafe { *size = meta.len().min(i32::MAX as u64) as i32 };
    }

    let mut flags = RETRO_VFS_STAT_IS_VALID;
    if meta.is_dir() {
        flags |= RETRO_VFS_STAT_IS_DIRECTORY;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if meta.file_type().is_char_device() {
            flags |= RETRO_VFS_STAT_IS_CHARACTER_SPECIAL;
        }
    }
    flags as c_int
}

unsafe extern "C" fn mkdir(dir: *const c_char) -> c_int {
    let Some(p) = (unsafe { to_path(dir) }) else {
        return -1;
    };
    // `create_dir_all` is happy with a directory that already exists, but the
    // API distinguishes the two cases and callers branch on -2, so check first.
    if p.is_dir() {
        return -2;
    }
    match std::fs::create_dir_all(&p) {
        Ok(()) => 0,
        // Lost a race with someone else creating it: still "already exists".
        Err(_) if p.is_dir() => -2,
        Err(_) => -1,
    }
}

// - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - - -
// Directories

struct DirHandle {
    /// Read in full at `opendir`. The API only promises a name stays valid
    /// until the next `readdir`, but holding the whole listing is simpler and
    /// keeps the names alive for the life of the handle, which is stricter.
    entries: Vec<(CString, bool)>,
    /// Index of the current entry, or `None` before the first `readdir`.
    pos: Option<usize>,
}

unsafe fn dir<'a>(dirstream: *mut retro_vfs_dir_handle) -> Option<&'a mut DirHandle> {
    (!dirstream.is_null()).then(|| unsafe { &mut *(dirstream as *mut DirHandle) })
}

unsafe extern "C" fn opendir(
    dir: *const c_char,
    include_hidden: bool,
) -> *mut retro_vfs_dir_handle {
    let Some(p) = (unsafe { to_path(dir) }) else {
        return std::ptr::null_mut();
    };
    let Ok(iter) = std::fs::read_dir(&p) else {
        return std::ptr::null_mut();
    };

    let mut entries = Vec::new();
    for entry in iter.flatten() {
        let name = entry.file_name();
        // "." and ".." are never listed: `read_dir` already omits them, and
        // handing them back only invites a caller to recurse into itself.
        let hidden = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                name.as_bytes().first() == Some(&b'.')
            }
            #[cfg(not(unix))]
            {
                name.to_string_lossy().starts_with('.')
            }
        };
        if hidden && !include_hidden {
            continue;
        }
        // `file_type()` doesn't follow symlinks; resolve them so a symlinked
        // directory lists as a directory, matching what `stat` above reports.
        let is_dir = entry
            .file_type()
            .map(|t| {
                if t.is_symlink() {
                    entry.path().is_dir()
                } else {
                    t.is_dir()
                }
            })
            .unwrap_or(false);

        let cname = {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                CString::new(name.as_bytes())
            }
            #[cfg(not(unix))]
            {
                CString::new(name.to_string_lossy().into_owned())
            }
        };
        // A name with an interior NUL can't be expressed in this API; skipping
        // it is the only option, and it can't name a real file anyway.
        if let Ok(cname) = cname {
            entries.push((cname, is_dir));
        }
    }

    Box::into_raw(Box::new(DirHandle { entries, pos: None })) as *mut retro_vfs_dir_handle
}

unsafe extern "C" fn readdir(dirstream: *mut retro_vfs_dir_handle) -> bool {
    let Some(h) = (unsafe { dir(dirstream) }) else {
        return false;
    };
    let next = match h.pos {
        None => 0,
        Some(i) => i + 1,
    };
    if next >= h.entries.len() {
        // Park past the end so repeated calls keep returning false rather than
        // wrapping back to the first entry.
        h.pos = Some(h.entries.len());
        return false;
    }
    h.pos = Some(next);
    true
}

unsafe extern "C" fn dirent_get_name(dirstream: *mut retro_vfs_dir_handle) -> *const c_char {
    let Some(h) = (unsafe { dir(dirstream) }) else {
        return std::ptr::null();
    };
    match h.pos.and_then(|i| h.entries.get(i)) {
        Some((name, _)) => name.as_ptr(),
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn dirent_is_dir(dirstream: *mut retro_vfs_dir_handle) -> bool {
    let Some(h) = (unsafe { dir(dirstream) }) else {
        return false;
    };
    h.pos
        .and_then(|i| h.entries.get(i))
        .is_some_and(|(_, is_dir)| *is_dir)
}

unsafe extern "C" fn closedir(dirstream: *mut retro_vfs_dir_handle) -> c_int {
    if dirstream.is_null() {
        return -1;
    }
    drop(unsafe { Box::from_raw(dirstream as *mut DirHandle) });
    0
}

#[cfg(test)]
#[path = "tests/vfs_tests.rs"]
mod tests;
