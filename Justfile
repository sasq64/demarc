
test:
    cargo test

clippy:
    cargo clippy

coverage:
    cargo llvm-cov --ignore-run-fail --html --open

coverage_text:
    cargo llvm-cov ---ignore-run-fail

# Run with Bevy's per-system tracing spans and the change audit (src/profiling.rs).
# Writes trace.json; ~100 MB per second of capture, so keep the run short.
profile file="demos/rebels.adf":
    cargo build --profile release-fast --features profile
    TRACE_CHROME=trace.json ./target/release-fast/demarc --window {{file}}

# Rank the spans in a profile trace by self time. `--filter` narrows it further.
trace-summary file="trace.json":
    scripts/trace-summary.py {{file}} --filter 'system: '

RUST_SYSROOT := `rustc --print sysroot`

perf:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib target/debug/client

cachegrind_debug:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib valgrind --tool=cachegrind target/debug/client

cachegrind:
  CARGO_MANIFEST_DIR=. valgrind --tool=cachegrind target/release-fast/demarc

run file="demos/rebels.adf":
    cargo run --profile release-fast -- --shuffle {{file}}

gb:
    cargo run --profile release-fast -- --scale 4 demos/nightmode.gb

c64:
    cargo run --profile release-fast -- demos/quantum_icc2026_v1p.prg

ami:
    cargo run --profile release-fast -- demos/rebels.adf

iff:
    cargo run --profile release-fast -- -C testdata/test.iff

royale file="demos/rebels.adf":
    cargo run --profile release-fast -- --shuffle --slangp slang-shaders/crt/crt-royale.slangp {{file}}

# Build the PCem libretro core out of external/pcem. PCem itself is GPLv2 and
# is not shipped with demarc; point demarc at the result with
# DEMARC_CORE_DIR, or copy it into <system dir>/cores.
#
# BIOS ROMs are not included and never will be: put them under
# <system dir>/pcem/roms/<machine>/ (external/pcem/docs/roms.txt lists what
# each machine needs).
pcem-core:
    cmake -B external/pcem/build-lr -G Ninja -S external/pcem \
        -DPCEM_DISPLAY_ENGINE=libretro -DCMAKE_BUILD_TYPE=Release
    ninja -C external/pcem/build-lr
    @echo "core at external/pcem/build-lr/src/pcem_libretro.so"

# Run a PCem machine config through the locally built core.
pc file:
    DEMARC_CORE_DIR={{justfile_directory()}}/external/pcem/build-lr/src \
        cargo run --profile release-fast -- {{file}}

install:
    cargo build --release
    sudo cp target/release/demarc /usr/local/bin

# What a `git tag v<version>` push would produce (no build).
release-check:
    dist plan

# Build this host's release artifacts into target/distrib, as CI would.
release-local:
    dist build --artifacts=local

HOME := x'${HOME}'
ZOLA := HOME / "projects/docs/minnberg"

# The dialog driver that runs inside wine alongside a Windows demo (source in
# tools/autodlg). It is a checked-in binary, so a change to it only reaches
# demarc once this has been run and system/win/demarc-autodlg.exe committed.
# `+crt-static` so the driver depends on nothing but wine's own kernel32 and
# user32: it has to start in whatever state the demo's prefix happens to be in.
autodlg:
    cd tools/autodlg && RUSTFLAGS="-C target-feature=+crt-static" cargo xwin build --release --target x86_64-pc-windows-msvc
    cp tools/autodlg/target/x86_64-pc-windows-msvc/release/demarc-autodlg.exe system/win/

# `-mssse3 -maes`: the vendored unrar C++ sources (unarc-rs -> unrar -> unrar_sys)
# tag their SSE/AES-NI routines with `__attribute__((target(...)))` only under
# `#ifdef __GNUC__`, which clang-cl doesn't define, so clang rejects the
# intrinsics unless the features are on for the whole translation unit. Both are
# runtime-dispatched inside unrar; SSSE3/AES-NI are a 2006/2010 baseline.
# scripts/prepare-xwin.sh patches the SDK cache -- see the comments there.
win:
    ./scripts/prepare-xwin.sh
    CXXFLAGS="-mssse3 -maes" cargo xwin build --release --target x86_64-pc-windows-msvc
    cp target/x86_64-pc-windows-msvc/release/demarc.exe {{ZOLA}}/static/dl/

site:
    cp demarc.md {{ZOLA}}/content/
    zola -r {{ZOLA}} build
    rsync -avz {{ZOLA}}/public/ sasq@minnberg.se:/var/www/html/

# Hyprland >= 0.5x parses its config as Lua, and `hyprctl keyword` only works
# with the legacy parser ("keyword can't work with non-legacy parsers. Use
# eval."), so drive the monitor through `hyprctl eval` instead.
pal:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "2880x1920@50", position = "auto", scale = 2 })'

ntsc:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "2880x1920@60", position = "auto", scale = 2 })'

# Back to the panel's native refresh rate.
native:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "preferred", position = "auto", scale = 2 })'
