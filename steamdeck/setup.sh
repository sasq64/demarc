#!/usr/bin/env bash
# Runs on the Deck, from ~/demarc, after install.sh / install.bat unpacked it:
# sets up wine and its prefix and adds the launcher to Steam. See
# docs/STEAMDECK.md.
set -euo pipefail

cd "$(dirname "$0")"
# A workstation build runs on the glibc it was built against (SteamOS has 2.39).
demarc() { ./glibc/ld-linux-x86-64.so.2 --library-path ./glibc:/usr/lib ./demarc-dev "$@"; }
echo "Setting up wine, see ~/demarc/setup-wine.log"
PATH="$PWD/bin:$PATH" demarc --setup-wine >setup-wine.log 2>&1 ||
    echo "demarc --setup-wine failed, see ~/demarc/setup-wine.log"

if ! grep -qs demarc.sh ~/.local/share/Steam/userdata/*/config/shortcuts.vdf; then
    DISPLAY=:0 steamos-add-to-steam "$PWD/demarc.sh"
fi

demarc --check-wine || true
