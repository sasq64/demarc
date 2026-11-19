use std::path::PathBuf;

fn main() {
    build_cbmconvert();
    build_adflib();
    build_dms();
    match std::env::var("CARGO_CFG_TARGET_OS").unwrap().as_str() {
        "android" => android_unrar_link(),
        // `unrar_sys` is C++ and does not ask for the runtime on iOS.
        "ios" => println!("cargo:rustc-link-lib=c++"),
        _ => {}
    }
}

/// `unrar_sys` asks for `-lpthread`, which bionic folds into libc, and links
/// no C++ runtime of its own.
fn android_unrar_link() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out.join("libpthread.a"), b"!<arch>\n").unwrap();
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=c++_static");
    println!("cargo:rustc-link-lib=c++abi");
}

/// Compile the `cbmconvert` command-line tool as a static library linked into
/// the binary. `main` is renamed to `cbmconvert_main` so we can call it from
/// Rust (see src/cbmconvert.rs) instead of running an external executable.
///
/// We build the C sources directly rather than via cbmconvert's CMakeLists.txt:
/// the tool is a flat set of `.c` files with no configuration step.
fn build_cbmconvert() {
    const DIR: &str = "../../external/cbmconvert";
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

/// Compile ADFlib (external/ADFlib) plus our own `src/c_shims/adf_unpack_shim.c` into a
/// static library, for the `--unadf` path in src/adf.rs.
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
    const DIR: &str = "../../external/ADFlib/src";
    const SHIM: &str = "src/c_shims/adf_unpack_shim.c";

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
    // libc ones are there. The MSVC UCRT only has strnlen, and linking our own
    // copy against it is a duplicate symbol.
    build.define("HAVE_STRNLEN", Some("1"));
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        for probe in ["HAVE_STRNDUP", "HAVE_STPNCPY", "HAVE_MEMPCPY"] {
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
/// `src/c_shims/dms_unpack_shim.c` into a static library, for src/dms.rs.
///
/// The sources are xDMS 1.3 (public domain) as amiberry carries them, with the
/// `.cpp` extension dropped -- they are plain C, and building them as C keeps
/// the binary off libstdc++. Only `pfile.c` needed editing, to read and write
/// stdio streams instead of amiberry's `struct zfile`; the header of that file
/// says what else changed.
fn build_dms() {
    const DIR: &str = "../../external/dms";
    const SHIM: &str = "src/c_shims/dms_unpack_shim.c";

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
