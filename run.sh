#!/usr/bin/env bash
RUST_LOG=demarc=debug cargo run --profile release-fast -- "$@"
# RUST_LOG=demarc=debug,retro=debug cargo run --profile release-fast -- "$@"
