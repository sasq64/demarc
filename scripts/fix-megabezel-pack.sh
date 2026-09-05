#!/usr/bin/env bash
# Repair the broken references in TheNamec's Mega Bezel Commodore pack.
#
# The pack (RC4.1) ships a few references that do not resolve, and RetroArch
# hides them: it only warns about a reference it cannot open and carries on.
# librashader does the same since the fork demarc builds against, so the
# affected presets load either way — they just quietly lose the settings the
# missing file was supposed to contribute. This puts the settings back.
#
# Safe to re-run, and worth re-running after updating the pack or slang-shaders.
#
# Usage: scripts/fix-megabezel-pack.sh [pack-dir]

set -euo pipefail

pack="${1:-shaders/Mega_Bezel_Packs/TheNamec-Commodore}"
[ -d "$pack/res/thenamec" ] || { echo "not a TheNamec pack: $pack" >&2; exit 1; }

# 1. The device folders for four monitors are referenced as Commodore_Commodore_*
#    (presets/*/<device>/NMC_SOFT_SMOOTH-SUPER-XBR/*.slangp) but shipped under
#    Commodore_*. Without these the SUPER-XBR presets lose their whole device
#    setup: bezel scaling, curvature, screen geometry.
for dev in C1084S-D1 C1084S-D2 C1201 C1702; do
    src="$pack/res/thenamec/devices/Commodore_$dev"
    dst="$pack/res/thenamec/devices/Commodore_Commodore_$dev"
    [ -d "$src" ] || { echo "skip: no $src" >&2; continue; }
    [ -e "$dst" ] || ln -s "Commodore_$dev" "$dst"
done

# 2. crt_spice/gtu/sharp/preset.params is shipped as "preset .params", with a
#    space, so the two NMC_SOFT_RGB flavours lose their GTU sharpness pass.
gtu="$pack/res/thenamec/shaders/crt_spice/gtu/sharp"
if [ -e "$gtu/preset .params" ] && [ ! -e "$gtu/preset.params" ]; then
    ln -s "preset .params" "$gtu/preset.params"
fi

# 3. The NMC_SOFT_VECTOR connector points at a Mega Bezel preset that was
#    renamed in Mega Bezel V1.7.0 (Dec 2022), after this pack was released.
#    Without it those presets end up with no shader passes at all.
vector="$pack/res/thenamec/shaders/base/mbz_vars_vector-horiz_std/preset.slangp"
if [ -f "$vector" ]; then
    sed -i 's|Presets/Variations/Vector-Horizontal__STD\.slangp|Presets/Variations/Vector/Vector-Color-HighResMode__STD.slangp|' "$vector"
fi

echo "patched $pack"
