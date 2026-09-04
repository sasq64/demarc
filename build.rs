use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

fn main() {
    cc::Build::new()
        .file("src/retro_log_shim.c")
        .compile("retro_log_shim");
    println!("cargo:rerun-if-changed=src/retro_log_shim.c");

    build_unrar_isnt_shim();
    build_cbmconvert();
    build_adflib();
    build_dms();
    build_system_zip();
}

/// Supply the two `isnt.cpp` symbols that `unrar_sys` leaves out when the build
/// host isn't Windows. See src/unrar_isnt_shim.cpp for the full story; the short
/// version is that its build script gates that file on `cfg!(windows)`, which is
/// the host, so cross-compiled Windows builds fail to link.
fn build_unrar_isnt_shim() {
    const SRC: &str = "src/unrar_isnt_shim.cpp";
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

/// Compile the `cbmconvert` command-line tool as a static library linked into
/// the binary. `main` is renamed to `cbmconvert_main` so we can call it from
/// Rust (see src/cbmconvert.rs) instead of running an external executable.
///
/// We build the C sources directly rather than via cbmconvert's CMakeLists.txt:
/// the tool is a flat set of `.c` files with no configuration step.
fn build_cbmconvert() {
    const DIR: &str = "cbmconvert";
    // The source set the upstream Makefile links into the `cbmconvert` binary.
    const SRCS: &[&str] = &[
        "main.c",
        "util.c",
        "read.c",
        "write.c",
        "lynx.c",
        "unark.c",
        "unarc.c",
        "t64.c",
        "c2n.c",
        "image.c",
        "archive.c",
    ];

    let mut build = cc::Build::new();
    build.include(DIR);
    // The code predates C99 `bool`; force a standard where its own `bool` enum
    // in util.h is legal (modern compilers default to C23, which reserves it).
    build.flag_if_supported("-std=gnu11");
    build.warnings(false);
    // Rename the CLI entry point so it can coexist with Rust's `main`.
    build.define("main", Some("cbmconvert_main"));

    for src in SRCS {
        let path = format!("{DIR}/{src}");
        println!("cargo:rerun-if-changed={path}");
        build.file(path);
    }
    build.compile("cbmconvert");
}

/// Compile ADFlib (external/ADFlib) plus our own `src/adf_unpack_shim.c` into a
/// static library, for the `--unadf` path in src/newsys/adf.rs.
///
/// ADFlib normally configures itself with autotools or CMake, both of which
/// exist only to write a `config.h` full of `HAVE_*` probes. `adf_util.h`
/// includes that header unless `BUILDING_WITH_CMAKE` is defined, so we define
/// it and answer the handful of probes that actually matter ourselves.
///
/// Only `src/*.c` is built. The `generic/`, `linux/` and `win32/`
/// subdirectories hold the *native* device driver, which reads real floppy
/// hardware; `adfLibInit` registers the portable "dump" driver that reads .adf
/// files, and that is the only one we ever ask for.
fn build_adflib() {
    const DIR: &str = "external/ADFlib/src";
    const SHIM: &str = "src/adf_unpack_shim.c";

    let Ok(sources) = std::fs::read_dir(DIR) else {
        // The library is vendored, not a submodule, so a checkout without it
        // should still build -- `--unadf` then reports it is unavailable.
        println!("cargo:warning=external/ADFlib not found, --unadf will be unavailable");
        return;
    };

    let mut build = cc::Build::new();
    build.include(DIR);
    build.warnings(false);
    // Skip the autotools-generated config.h (see above).
    build.define("BUILDING_WITH_CMAKE", None);

    // ADFlib carries its own strnlen/strndup/stpncpy/mempcpy for platforms
    // without them, declared `static` in adf_util.c. On glibc those names are
    // already declared non-static by <string.h>, and the two collide ("static
    // declaration of 'mempcpy' follows non-static declaration"), so tell it the
    // libc ones are there. MSVC has none of the four, so it keeps its own.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        for probe in ["HAVE_STRNLEN", "HAVE_STRNDUP", "HAVE_STPNCPY", "HAVE_MEMPCPY"] {
            build.define(probe, Some("1"));
        }
    }

    let mut files: Vec<PathBuf> = sources
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    // read_dir order is the file system's; sort so the build is reproducible.
    files.sort();

    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        build.file(file);
    }
    println!("cargo:rerun-if-changed={SHIM}");
    build.file(SHIM);
    build.compile("adflib");
}

/// Compile the DMS unpacker (external/dms) plus our own
/// `src/dms_unpack_shim.c` into a static library, for src/newsys/dms.rs.
///
/// The sources are xDMS 1.3 (public domain) as amiberry carries them, with the
/// `.cpp` extension dropped -- they are plain C, and building them as C keeps
/// the binary off libstdc++. Only `pfile.c` needed editing, to read and write
/// stdio streams instead of amiberry's `struct zfile`; the header of that file
/// says what else changed.
fn build_dms() {
    const DIR: &str = "external/dms";
    const SHIM: &str = "src/dms_unpack_shim.c";

    let mut build = cc::Build::new();
    build.include(DIR);
    // Twenty-five year old C: `Unpack_Track` and friends trip -Wall constantly
    // and there is nothing to be done about it in code we want to keep diffable
    // against upstream.
    build.warnings(false);

    let mut files: Vec<PathBuf> = std::fs::read_dir(DIR)
        .expect("external/dms not found")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    // read_dir order is the file system's; sort so the build is reproducible.
    files.sort();

    for file in &files {
        println!("cargo:rerun-if-changed={}", file.display());
        build.file(file);
    }
    println!("cargo:rerun-if-changed={SHIM}");
    build.file(SHIM);
    build.compile("dms");
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
/// against a subtree whose file names are the user's. PCem is the case in
/// point: it looks for BIOS ROMs under `pcem/roms/` (copyrighted, so never
/// ours to ship) and writes NVR, logs and configs into `pcem/` beside them,
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
    "system/pcem",
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
    if collect_entries(Path::new("system"), &mut entries) {
        // Nothing under `system/` is written at runtime, so the whole tree can
        // be watched with one line. (It never is in practice -- see the note in
        // `collect_entries` -- but then the walk has emitted the lines itself.)
        println!("cargo:rerun-if-changed=system");
    }
    entries.sort();

    // Re-running this script is not the same as the archive needing to change:
    // a touched `cbmconvert/*.c`, an edit to this file, or a `cargo clean`d
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
        let name = path.to_string_lossy().replace('\\', "/");
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
        hasher.update(path.to_string_lossy().replace('\\', "/").as_bytes());
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
/// `system/amiga/` or `system/pcem/` -- exactly the paths the archive is
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
            let rel = path.to_string_lossy().replace('\\', "/");
            if SKIP_DIRS.contains(&rel.as_ref()) {
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
