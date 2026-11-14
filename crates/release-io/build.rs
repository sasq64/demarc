fn main() {
    build_cbmconvert();
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
