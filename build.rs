use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

fn main() {
    cc::Build::new()
        .file("src/retro_log_shim.c")
        .compile("retro_log_shim");
    println!("cargo:rerun-if-changed=src/retro_log_shim.c");

    build_unrar_isnt_shim();
    build_cbmconvert();
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

const MARKER_FILES: &[&str] = &[
    ".v4",
    ".checksum",
    "vicerc-dump-C64",
    "vicerc-dump-C64SC",
    "vicerc-dump-PLUS4",
    "pcsx-card2.mcd",
    // Amiberry's own settings file, written into `system/amiga/` as it runs.
    "amiberry.ini",
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
    println!("cargo:rerun-if-changed=system");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let zip_path = Path::new(&out_dir).join("system.zip");
    let file = File::create(&zip_path).expect("Failed to create system.zip");
    let mut writer = zip::ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    // Collect entries first and sort them so the archive layout is stable
    // across builds.
    let mut entries = Vec::new();
    collect_entries(Path::new("system"), &mut entries);
    entries.sort();

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
    let digest = Sha256::digest(&bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=SYSTEM_ZIP_CHECKSUM={hex}");
}

/// Recursively gather every directory and (non-marker) file under `dir`,
/// skipping the subtrees named by [`SKIP_DIRS`].
fn collect_entries(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let rel = path.to_string_lossy().replace('\\', "/");
            if SKIP_DIRS.contains(&rel.as_ref()) {
                continue;
            }
            out.push(path.clone());
            collect_entries(&path, out);
        } else if !MARKER_FILES.contains(&entry.file_name().to_string_lossy().as_ref()) {
            out.push(path);
        }
    }
}
