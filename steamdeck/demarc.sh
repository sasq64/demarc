#!/bin/sh
# Launcher for Steam (Game Mode). Arguments come from the shortcut launch options.
# See docs/STEAMDECK.md for what each line is for.
cd "$(dirname "$0")"
export WINIT_X11_SCALE_FACTOR=1
# SteamOS ships an older gamescope Vulkan layer that cannot talk to demarc's gamescope.
export DISABLE_GAMESCOPE_WSI=1
# Without that layer a demo that does not ask for vsync renders flat out.
export DXVK_CONFIG="d3d9.presentInterval=1;dxgi.syncInterval=1"
# What Steam injects into a game would also land in demarc's own gamescope and
# in the demo inside it: the overlay drawn twice, and the outer gamescope's
# stats and limiter files shared.
unset LD_PRELOAD ENABLE_VK_LAYER_VALVE_steam_overlay_1 ENABLE_VK_LAYER_VALVE_steam_fossilize_1
unset GAMESCOPE_STATS GAMESCOPE_LIMITER_FILE GAMESCOPE_MODE_SAVE_FILE GAMESCOPE_WAYLAND_DISPLAY
unset MANGOHUD_CONFIGFILE STEAM_USE_MANGOAPP RADV_FORCE_VRS_CONFIG_FILE
# SteamOS has no ASF demuxer or WMA decoder; install.sh puts them here.
export GST_PLUGIN_PATH="$HOME/.local/share/demarc/gst"
# The gamescope core's own libgudev needs a newer glib than SteamOS 3.6 has.
for f in "$HOME"/.cache/demarc/cores/*/libgudev-1.0.so.0 "$HOME"/.cache/demarc/cores/*/lib/libgudev-1.0.so.0; do
    [ -f "$f" ] && mv "$f" "$f.bundled"
done
# test-env, test-args: one-off environment and arguments for a run started over ssh.
[ -f test-env ] && . ./test-env
[ -f test-args ] && set -- $(cat test-args)
if [ $# -eq 0 ]; then
    # The panel's own size: the GPU is pegged at the 1920x1080 default.
    set -- demozoo.txt.gz csdb.txt.gz --select -x wine_res=1280x800
fi
# Own IPC namespace: demarc's gamescope otherwise sends its frame stats to the
# same message queue as the Deck's, and the performance HUD jumps between the two.
# The private socket directory is for the inner Xwayland, which refuses one it does not own.
X=/tmp/.X11-unix
# A workstation build runs on the glibc it was built against (SteamOS has 2.39).
exec bwrap --dev-bind / / --tmpfs $X --bind $X/X0 $X/X0 --bind $X/X1 $X/X1 --unshare-ipc --die-with-parent -- \
    ./glibc/ld-linux-x86-64.so.2 --library-path ./glibc:/usr/lib ./demarc-dev "$@" >demarc.log 2>&1
