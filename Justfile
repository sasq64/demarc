
test:
    cargo test

clippy:
    cargo clippy

coverage:
    cargo llvm-cov --ignore-run-fail --html --open

coverage_text:
    cargo llvm-cov ---ignore-run-fail

RUST_SYSROOT := `rustc --print sysroot`

perf:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib target/debug/client

cachegrind_debug:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib valgrind --tool=cachegrind target/debug/client

cachegrind:
  CARGO_MANIFEST_DIR=. valgrind --tool=cachegrind target/release/client

gb:
    cargo run --profile release-fast -- demos/nightmode.gb

c64:
    cargo run --profile release-fast -- demos/quantum_icc2026_v1p.prg

ami:
    cargo run --profile release-fast -- demos/rebels.adf

royale:
    cargo run --profile release-fast -- --slangp slang-shaders/crt/crt-royale.slangp demos/rebels.adf

install:
    cargo build --release
    sudo cp target/release/demarc /usr/local/bin

HOME := x'${HOME}'
ZOLA := HOME / "projects/docs/minnberg"

win:
    cargo xwin build --release --target x86_64-pc-windows-msvc
    cp target/x86_64-pc-windows-msvc/release/demarc.exe {{ZOLA}}/static/dl/

site:
    cp demarc.md {{ZOLA}}/content/
    zola -r {{ZOLA}} build
    rsync -avz {{ZOLA}}/public/ sasq@minnberg.se:/var/www/html/

pal:
    hyprctl keyword monitor "eDP-1,2880x1920@50,auto,2"

ntsc:
    hyprctl keyword monitor "eDP-1,2880x1920@60,auto,2"
