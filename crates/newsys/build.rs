use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

/// Everything this script reads — `external/`, `system/` — lives at the repo
/// root, two levels above this crate. Paths stay relative so `rerun-if-changed`
/// keeps working; `rel_name` strips the prefix back off wherever a path is used
/// as a name rather than opened.
const ROOT: &str = "../..";

/// Repo-root-relative form of `path`: what `system.zip` stores, what
/// `system_dir()` extracts, and what `SKIP_DIRS` is written in.
fn rel_name(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.strip_prefix("../../").unwrap_or(&s).to_string()
}

fn main() {
    build_unrar_isnt_shim();
    build_system_zip();
}

/// Supply the two `isnt.cpp` symbols that `unrar_sys` leaves out when the build
/// host isn't Windows. See src/c_shims/unrar_isnt_shim.cpp for the full story; the short
/// version is that its build script gates that file on `cfg!(windows)`, which is
/// the host, so cross-compiled Windows builds fail to link.
fn build_unrar_isnt_shim() {
    const SRC: &str = "src/c_shims/unrar_isnt_shim.cpp";
    println!("cargo:rerun-if-changed={SRC}");

    let target_windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !target_windows || cfg!(windows) {
        return;
    }

    cc::Build::new()
        .cpp(true)
        .file(SRC)
        .compile("unrar_isnt_shim");
}

const MARKER_FILES: &[&str] = &[
    ".v4",
    ".checksum",
    "vicerc-dump-C64",
    "vicerc-dump-C64SC",
    "vicerc-dump-PLUS4",
    "pcsx-card2.mcd",
    // Amiberry's own settings file, written into `system/amiga/` as it runs.
    "amiberry.ini",
    // libsc68 rewrites this on every exit to bump its `total_time` play
    // counter, so packing it made the archive -- and with it the whole crate --
    // dirty after every single run. Every setting the checked-in copy carried
    // was already libsc68's own default, and it regenerates the file with
    // those defaults when it isn't there, so there is nothing to ship.
    "sc68.cfg",
];

/// Directories under `system/` that are never packed, whatever they contain.
///
/// `MARKER_FILES` can only name files it knows in advance, which is no help
/// against a subtree whose file names are the user's. 86Box is the case in
/// point: it looks for BIOS ROMs under `86box/roms/` (copyrighted, so never
/// ours to ship) and writes NVR, logs and configs into `86box/` beside them,
/// with names taken from whichever machine config was loaded. In a debug build
/// `system_dir()` is this very directory, so without this the emulator would
/// be filling up the archive as it ran.
///
/// The `amiga/` entries are the same story with amiberry, which treats its
/// system dir as its home: it scratch-builds a boot drive under `WHDBoot/tmp/`
/// and stores WHDLoad savegames under `WHDBoot/save-data/` and `WHDSaves/`.
/// `WHDBoot/tmp/` and `save-data/Kickstarts/` are *symlink farms* pointing back
/// up at the Kickstart ROMs, so packing them does not merely ship a developer's
/// savegames — move `system/amiga/` and the build breaks on the dangling links.
const SKIP_DIRS: &[&str] = &[
    "system/86box",
    "system/amiga/WHDBoot/save-data",
    "system/amiga/WHDBoot/tmp",
    "system/amiga/WHDSaves",
    "system/amiga/Visuals",
];

/// Pack the loose `system/` directory into `system.zip` (embedded into the
/// binary via `include_bytes!`), then emit a SHA-256 of the resulting archive
/// as the `SYSTEM_ZIP_CHECKSUM` env var. The runtime writes that checksum next
/// to the extracted files and re-extracts whenever it no longer matches, which
/// replaces the old hand-maintained `.v4` marker.
fn build_system_zip() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let zip_path = out_dir.join("system.zip");
    let stamp_path = out_dir.join("system.zip.stamp");

    // Collect entries first and sort them so the archive layout is stable
    // across builds.
    let mut entries = Vec::new();
    if collect_entries(&Path::new(ROOT).join("system"), &mut entries) {
        // Nothing under `system/` is written at runtime, so the whole tree can
        // be watched with one line. (It never is in practice -- see the note in
        // `collect_entries` -- but then the walk has emitted the lines itself.)
        println!("cargo:rerun-if-changed={ROOT}/system");
    }
    entries.sort();

    // Re-running this script is not the same as the archive needing to change:
    // a touched `external/cbmconvert/*.c`, an edit to this file, or a `cargo clean`d
    // OUT_DIR bring us here too, and deflating 19 MB of `system/` costs ~3s.
    // Skip that when the inputs fingerprint the same as the archive we left in
    // OUT_DIR last time.
    let fingerprint = input_fingerprint(&entries);
    if let Ok(stamp) = std::fs::read_to_string(&stamp_path)
        && let Some((cached, checksum)) = stamp.split_once(' ')
        && cached == fingerprint
        && zip_path.is_file()
    {
        println!("cargo:rustc-env=SYSTEM_ZIP_CHECKSUM={checksum}");
        return;
    }

    let file = File::create(&zip_path).expect("Failed to create system.zip");
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for path in &entries {
        let name = rel_name(path);
        if path.is_dir() {
            writer
                .add_directory(name, options)
                .expect("Failed to add directory to system.zip");
        } else {
            writer
                .start_file(name, options)
                .expect("Failed to start file in system.zip");
            let mut buf = Vec::new();
            File::open(path)
                .and_then(|mut f| f.read_to_end(&mut buf))
                .expect("Failed to read system file");
            writer
                .write_all(&buf)
                .expect("Failed to write file into system.zip");
        }
    }
    writer.finish().expect("Failed to finalize system.zip");

    let bytes = std::fs::read(&zip_path).expect("Failed to read back system.zip");
    let hex = hex(&Sha256::digest(&bytes));
    std::fs::write(&stamp_path, format!("{fingerprint} {hex}")).expect("Failed to write zip stamp");
    println!("cargo:rustc-env=SYSTEM_ZIP_CHECKSUM={hex}");
}

/// A cheap fingerprint of the archive's inputs: every packed path with its
/// length and mtime, in the order they are packed. Deliberately does not read
/// the files -- this runs on every build script invocation, and a stale mtime
/// only costs a needless repack.
fn input_fingerprint(entries: &[PathBuf]) -> String {
    let mut hasher = Sha256::new();
    for path in entries {
        hasher.update(rel_name(path).as_bytes());
        if let Ok(meta) = path.metadata() {
            hasher.update(meta.len().to_le_bytes());
            if let Ok(mtime) = meta.modified()
                && let Ok(since_epoch) = mtime.duration_since(UNIX_EPOCH)
            {
                hasher.update(since_epoch.as_nanos().to_le_bytes());
            }
        }
        hasher.update(b"\n");
    }
    hex(&hasher.finalize())
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Recursively gather every directory and (non-marker) file under `dir`,
/// skipping the subtrees named by [`SKIP_DIRS`], and emit the
/// `rerun-if-changed` lines that watch what was gathered.
///
/// Returns whether the subtree is *pristine*: free of both [`MARKER_FILES`] and
/// [`SKIP_DIRS`], i.e. nothing under it is written while the emulator runs.
///
/// The distinction is what keeps no-op builds fast. Cargo watches a directory
/// by taking the newest mtime found anywhere beneath it, so the single
/// `rerun-if-changed=system` this used to emit fired on every scribble into
/// `system/amiga/` or `system/86box/` -- exactly the paths the archive is
/// careful *not* to contain. Re-running a build script dirties the crate that
/// owns it, so one Amiga demo made the next `cargo build` a ~10s rebuild of
/// demarc with nothing to show for it.
///
/// A pristine subtree can still be watched with one directory line, which has
/// the nice property of also catching files being added and removed. A subtree
/// that is not gets watched entry by entry instead. The gap that leaves: a file
/// added directly to one of the handful of non-pristine directories (`system/`
/// itself, `system/vice/`, `system/amiga/`, `system/amiga/WHDBoot/`) is not
/// noticed until something else triggers a repack -- `touch build.rs`.
fn collect_entries(dir: &Path, out: &mut Vec<PathBuf>) -> bool {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return true;
    };
    let mut pristine = true;
    // Paths to watch individually, but only if this directory turns out not to
    // be pristine; if it is, our caller's single line for `dir` covers them all.
    let mut watch_if_dirty = Vec::new();

    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let rel = rel_name(&path);
            if SKIP_DIRS.contains(&rel.as_str()) {
                pristine = false;
                continue;
            }
            out.push(path.clone());
            if collect_entries(&path, out) {
                watch_if_dirty.push(path);
            } else {
                // Not pristine, so it has already emitted its own lines.
                pristine = false;
            }
        } else if MARKER_FILES.contains(&entry.file_name().to_string_lossy().as_ref()) {
            pristine = false;
        } else {
            out.push(path.clone());
            watch_if_dirty.push(path);
        }
    }

    if !pristine {
        for path in &watch_if_dirty {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    pristine
}
