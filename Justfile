

build_release:
    cargo build --release 

gen_cert:
    openssl req -x509 \
        -newkey ec \
        -pkeyopt ec_paramgen_curve:prime256v1 \
        -keyout server/server.key \
        -out server/server.crt \
        -days 14 \
        -nodes \
        -subj "/CN=minnberg.se" \
        -addext "subjectAltName=DNS:localhost,DNS:minnberg.se,IP:188.166.54.191,IP:127.0.0.1,IP:::1"

check_wasm:
    CARGO_BUILD_TARGET=wasm32-unknown-unknown cargo check -p wasm-client

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

ami:
    cargo run --profile release-fast -- demos/rebels.adf

