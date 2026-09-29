#!/usr/bin/env bash
RUST_LOG=demarc=debug,newsys=debug,retro_core=debug,retro=debug cargo run --bin demarc --profile release-fast -- "$@"
# RUST_LOG=demarc=debug,retro=debug cargo run --profile release-fast -- "$@"
