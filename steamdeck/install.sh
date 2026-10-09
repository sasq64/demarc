#!/usr/bin/env bash
# Install demarc on a Steam Deck from this workstation, over ssh.
#
#   steamdeck/install.sh [user@host]
#
# Builds demarc here and copies it to ~/demarc on the Deck with the glibc it was
# built against, then on the Deck fetches wine, makes the wine prefix and adds
# the launcher to Steam. Steps already done are skipped, so it is also how a new
# build is pushed. See docs/STEAMDECK.md.
set -euo pipefail

HOST="${1:-deck@192.168.1.214}"
WINE_VERSION=11.17
WINE_URL="https://github.com/Kron4ek/Wine-Builds/releases/download/$WINE_VERSION/wine-$WINE_VERSION-amd64-wow64.tar.xz"
SSH_OPTS=(${DECK_SSH_OPTS:--o ControlMaster=auto -o ControlPath=/tmp/demarc-deck-%C -o ControlPersist=5m})

cd "$(dirname "$0")/.."
cargo build --profile release-fast

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/glibc" "$stage/prefix-setup/files"
strip -o "$stage/demarc-dev" target/release-fast/demarc
cp steamdeck/demarc.sh "$stage/"
for lib in ld-linux-x86-64.so.2 libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 \
    libresolv.so.2 librt.so.1 libgcc_s.so.1 libstdc++.so.6; do
    cp -L "/usr/lib/$lib" "$stage/glibc/"
done
cp demodb/demozoo.txt.gz demodb/csdb.txt.gz "$stage/"
cp scripts/mk_wine_prefix.sh "$stage/prefix-setup/"
cp files/*.dll files/*.acm files/gm.dls "$stage/prefix-setup/files/"
# SteamOS has no cabextract, which winetricks needs.
cp -L "$(command -v cabextract)" "$stage/prefix-setup/"

echo "Copying to $HOST:demarc"
chmod 755 "$stage"
rsync -a -e "ssh ${SSH_OPTS[*]}" "$stage/" "$HOST:demarc/"

ssh "${SSH_OPTS[@]}" "$HOST" WINE_URL="$WINE_URL" bash -s <<'REMOTE'
set -euo pipefail
wine=$HOME/.local/share/demarc/wine
if [ ! -x "$wine/bin/wine" ]; then
    echo "Fetching wine"
    mkdir -p "$wine"
    curl -fL "$WINE_URL" | tar -C "$wine" -xJ --strip-components=1
fi
cp ~/demarc/prefix-setup/cabextract "$wine/bin/"
export PATH="$wine/bin:$PATH"

if [ ! -d ~/.wine-demarc ]; then
    echo "Making the wine prefix. Reboot the Deck afterwards if its buttons stop responding."
    cd ~/demarc/prefix-setup
    DISPLAY=:0 WINEDLLOVERRIDES="mscoree=d;mshtml=d;wineusb.sys=d" ./mk_wine_prefix.sh >setup.log 2>&1 ||
        echo "mk_wine_prefix.sh failed, see ~/demarc/prefix-setup/setup.log"
fi

if ! grep -qs demarc.sh ~/.local/share/Steam/userdata/*/config/shortcuts.vdf; then
    DISPLAY=:0 steamos-add-to-steam ~/demarc/demarc.sh
fi

cd ~/demarc
./glibc/ld-linux-x86-64.so.2 --library-path ./glibc:/usr/lib ./demarc-dev --check-wine || true
REMOTE
