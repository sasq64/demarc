#!/usr/bin/env bash
RUST_LOG=demarc=debug cargo run --profile release-fast -- "$@"
