#!/usr/bin/env bash
# Render the installer's icon and bitmaps from demarc-logo/art into $1.
set -euo pipefail
OUT="$1"
ART="$(cd "$(dirname "$0")/../../demarc-logo/art" && pwd)"
BG="#15171B"
mkdir -p "$OUT"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

for s in 16 24 32 48 64 128 256; do
    rsvg-convert -w $s -h $s "$ART/icon.svg" -o "$tmp/icon-$s.png"
done
magick "$tmp"/icon-{16,24,32,48,64,128,256}.png "$OUT/demarc.ico"

# 164x314 is the MUI welcome/finish page bitmap, 150x57 the header.
rsvg-convert -w 112 "$ART/icon.svg" -o "$tmp/welcome-icon.png"
# logo.svg is a 1600x520 frame around the keycap; keep the keycap.
rsvg-convert -w 1600 "$ART/logo.svg" -o "$tmp/logo-full.png"
magick "$tmp/logo-full.png" -crop 820x290+390+115 +repage "$tmp/logo.png"
magick "$tmp/logo.png" -resize 148x "$tmp/welcome-logo.png"
magick -size 164x314 "xc:$BG" \
    "$tmp/welcome-icon.png" -gravity north -geometry +0+70 -composite \
    "$tmp/welcome-logo.png" -gravity north -geometry +0+205 -composite \
    -type TrueColor "BMP3:$OUT/welcome.bmp"

magick "$tmp/logo.png" -resize 140x50 "$tmp/header-logo.png"
magick -size 150x57 "xc:$BG" "$tmp/header-logo.png" -gravity center -composite \
    -type TrueColor "BMP3:$OUT/header.bmp"
