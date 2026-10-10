#!/usr/bin/env bash
# Build the Steam Deck installer: target/steamdeck/demarc-steamdeck.zip.
#
# The zip holds demarc with the glibc it was built against, and install.sh /
# install.bat, which copy it to a Deck over ssh. See docs/STEAMDECK.md.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --profile release-fast

out=target/steamdeck
rm -rf "$out"
stage=$out/payload
dist=$out/demarc-steamdeck
mkdir -p "$stage/glibc" "$stage/prefix-setup/files" "$dist"
strip -o "$stage/demarc-dev" target/release-fast/demarc
cp steamdeck/demarc.sh steamdeck/setup.sh "$stage/"
for lib in ld-linux-x86-64.so.2 libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 \
    libresolv.so.2 librt.so.1 libgcc_s.so.1 libstdc++.so.6; do
    cp -L "/usr/lib/$lib" "$stage/glibc/"
done
cp demodb/demozoo.txt.gz demodb/csdb.txt.gz "$stage/"
cp scripts/mk_wine_prefix.sh "$stage/prefix-setup/"
cp files/*.dll files/*.acm files/gm.dls "$stage/prefix-setup/files/"
# SteamOS has no cabextract, which winetricks needs.
cp -L "$(command -v cabextract)" "$stage/prefix-setup/"

tar -C "$stage" -czf "$dist/demarc.tar.gz" .
cp steamdeck/install.sh "$dist/"
sed 's/$/\r/' steamdeck/install.bat >"$dist/install.bat"
(cd "$out" && zip -qr demarc-steamdeck.zip demarc-steamdeck)
echo "$out/demarc-steamdeck.zip"
