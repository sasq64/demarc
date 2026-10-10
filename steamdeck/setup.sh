#!/usr/bin/env bash
# Runs on the Deck, from ~/demarc, after install.sh / install.bat unpacked it:
# fetches wine, makes the wine prefix and adds the launcher to Steam. Steps
# already done are skipped. See docs/STEAMDECK.md.
set -euo pipefail

WINE_VERSION=11.17
WINE_URL="https://github.com/Kron4ek/Wine-Builds/releases/download/$WINE_VERSION/wine-$WINE_VERSION-amd64-wow64.tar.xz"

cd "$(dirname "$0")"
wine=$HOME/.local/share/demarc/wine
if [ ! -x "$wine/bin/wine" ]; then
    echo "Fetching wine"
    mkdir -p "$wine"
    curl -fL "$WINE_URL" | tar -C "$wine" -xJ --strip-components=1
fi
cp prefix-setup/cabextract "$wine/bin/"
export PATH="$wine/bin:$PATH"

# WMA soundtracks: SteamOS's gstreamer has neither the ASF demuxer nor libav.
gst=$HOME/.local/share/demarc/gst
if [ ! -f "$gst/libgstlibav.so" ]; then
    echo "Fetching gstreamer plugins"
    mkdir -p "$gst"
    v=$(pacman -Q gstreamer | cut -d' ' -f2)
    for pkg in gst-plugins-ugly:libgstasf.so gst-libav:libgstlibav.so; do
        curl -fL "https://archive.archlinux.org/packages/g/${pkg%:*}/${pkg%:*}-$v-x86_64.pkg.tar.zst" |
            tar -C "$gst" --zstd -x --strip-components=3 "usr/lib/gstreamer-1.0/${pkg#*:}"
    done
fi

if [ ! -d ~/.wine-demarc ]; then
    echo "Making the wine prefix. Reboot the Deck afterwards if its buttons stop responding."
    (cd prefix-setup &&
        DISPLAY=:0 WINEDLLOVERRIDES="mscoree=d;mshtml=d;wineusb.sys=d" ./mk_wine_prefix.sh >setup.log 2>&1) ||
        echo "mk_wine_prefix.sh failed, see ~/demarc/prefix-setup/setup.log"
fi

if ! grep -qs demarc.sh ~/.local/share/Steam/userdata/*/config/shortcuts.vdf; then
    DISPLAY=:0 steamos-add-to-steam "$PWD/demarc.sh"
fi

./glibc/ld-linux-x86-64.so.2 --library-path ./glibc:/usr/lib ./demarc-dev --check-wine || true
