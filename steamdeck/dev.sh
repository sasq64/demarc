#!/usr/bin/env bash
# Build demarc, update the copy on a Deck that install.sh already set up and
# (re)start it there.
#
#   steamdeck/dev.sh [deck@<address of the Deck>]
set -euo pipefail

deck=${1:-${DECK:-deck@192.168.1.214}}
# The Steam shortcut for demarc.sh.
gameid=${DECK_GAMEID:-9770677391545335808}

cd "$(dirname "$0")/.."
cargo build --profile release-fast

stage=target/steamdeck/dev
mkdir -p "$stage/glibc"
strip -o "$stage/demarc-dev" target/release-fast/demarc
cp -p steamdeck/demarc.sh demodb/demozoo.txt.gz demodb/csdb.txt.gz "$stage/"
for lib in ld-linux-x86-64.so.2 libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 \
    libresolv.so.2 librt.so.1 libgcc_s.so.1 libstdc++.so.6; do
    cp -Lp "/usr/lib/$lib" "$stage/glibc/"
done

ssh "$deck" "pkill -x ld-linux-x86-64" || true
rsync -az --info=progress2 "$stage/" "$deck:demarc/"
ssh "$deck" "steam steam://rungameid/$gameid >/dev/null 2>&1"
