# Running a Windows demo at a resolution you don't know yet

Investigated 2026-08-31 against Orange's *The Nonstop Ibiza Experience*
(`pcdemo/ibiza/demo.exe`, OpenPTC + `ptc.dll`/`hermes.dll`), gamescope 3.16.25,
prefix `~/.wine-demarc64`.

## The problem

`gamescope -w 512 -h 384 -f -- wine demo.exe` fills the screen. Anything else —
a larger session, `--force-windows-fullscreen`, a wine virtual desktop — leaves
the demo as a small picture in the corner of a black screen. Since demarc does
not know a release's resolution before running it, `wine_emu.rs` currently
guesses (`PICK_RES = 1920x1200`) and accepts the small picture.

## Why it happens

**gamescope's nested screen size *is* the display mode wine sees, and no client
can change it at runtime.** Inside a session:

```
$ xrandr --output gamescope --mode 640x480
X Error of failed request:  BadMatch (invalid parameter attributes)
  Major opcode of failed request:  140 (RANDR)
  Minor opcode of failed request:  7 (RRSetScreenSize)
```

gamescope *does* advertise a mode list derived from `-w/-h` — a 1024x768 session
offers 320x200, 320x240, 640x350, 640x400, 640x480, 720x400, 800x600 … — so a
demo's `EnumDisplayModes` finds what it wants and its `SetDisplayMode` returns
`DD_OK`. The switch then quietly does not happen. `WINEDEBUG=+ddraw` at
`-w 1024 -h 768`:

```
ddraw2_SetDisplayMode iface ..., width 640, height 480, bpp 32
device_parent_mode_changed  Resizing window 0002005A to (0,0)-(1024,768)
  DDSD_CAPS   : ... DDSCAPS_PRIMARYSURFACE DDSCAPS_VISIBLE ...
  DDSD_HEIGHT : 768
  DDSD_WIDTH  : 1024
```

The window is snapped to the whole nested screen and ddraw hands the demo a
1024x768 primary surface. OpenPTC then writes its fixed 512x384 image centred in
the 640x480 area it believes it has, at the top-left of that surface — measured
from a headless screenshot: content at +64+45, 512x384, which is exactly
`(640-512)/2, (480-384)/2` inside a 640x480 origin at 0,0.

**gamescope only ever scales the whole nested screen to the output.** It does not
scale content that is smaller than the nested screen, and no flag makes it.

That kills the two obvious workarounds:

- `--force-windows-fullscreen` resizes the *X window* to the nested size. The
  demo still draws 512x384 into a corner of it, so this adds black rather than
  removing it.
- `wine explorer /desktop=name,WxH` does make the mode change real — with a
  desktop, `WINEDEBUG=+ddraw` shows `Resizing window ... to (0,0)-(640,480)` and
  a primary surface of `DDSD_WIDTH : 640 / DDSD_HEIGHT : 480`. But the *desktop
  window itself* keeps the size it was given, so the result is a small picture
  on a large blue wine desktop. A headless screenshot of
  `/desktop=demo,1024x768` is 1024x768 of wine desktop background.
- `GAMESCOPE_XWAYLAND_MODE_CONTROL` (root-window atom, `w, h, superRes`) can be
  set and reads back correctly, but did not move the reported mode in a headless
  session with no Steam integration. Not a runtime escape hatch here.

## The fix: probe headlessly, then run

The size has to be known before gamescope starts, so find it in a throwaway
session first. `gamescope --backend headless` puts nothing on screen and still
runs a full Xwayland + compositor, which gives two independent numbers:

1. **The requested mode**, from `WINEDEBUG=+ddraw,+d3d` → the first
   `SetDisplayMode iface ..., width W, height H`. Exact, and available within a
   few seconds of the demo starting — the probe can be killed on that line.
2. **The area actually drawn**, from a screenshot. Set
   `GAMESCOPECTRL_REQUEST_SCREENSHOT` on the nested root and gamescope writes
   `/tmp/gamescope.png`. Take a few, `-evaluate-sequence max` them (so a dark
   frame doesn't under-report), then `-fuzz 2% -trim`.

(2) is what recovers 512x384. That number exists *only in the pixels*: PTC locks
the primary surface and writes a centred rect, so no wine API call ever mentions
it — the ddraw log for the whole run contains no surface other than 1024x768.
Prefer (2), fall back to (1), then to a default.

Verified end to end on ibiza: probe reported `requested mode: 640x480`,
`drawn area: 512x384`, and chose `gamescope -w 512 -h 384 -f -S fit -F nearest`
— the same numbers that had to be supplied by hand. `-w 640 -h 480` also fills
the screen correctly, keeping the demo's own black border.

## Script

```bash
#!/usr/bin/env bash
# Run a Windows demo under wine+gamescope at whatever resolution it turns out to
# want. gamescope's nested screen size IS the display mode wine reports, and
# clients cannot change it at runtime (RRSetScreenSize -> BadMatch), so the size
# has to be known before gamescope starts. Phase 1 finds it in a throwaway
# headless session; phase 2 is the real run.
set -euo pipefail

EXE=${1:?usage: gs-wine.sh demo.exe}
DIR=$(cd "$(dirname "$EXE")" && pwd)
EXE=$(basename "$EXE")
PROBE_W=${PROBE_W:-1024} PROBE_H=${PROBE_H:-768}   # ceiling; also caps the mode list wine sees
WARMUP=${WARMUP:-12}                               # seconds before the first screenshot
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

probe() {
  cat > "$WORK/probe.sh" <<EOF
#!/bin/sh
cd "$DIR"
wine "$EXE" &
timeout $WARMUP tail -f /dev/null
for n in 1 2 3; do
  xprop -root -f GAMESCOPECTRL_REQUEST_SCREENSHOT 32c -set GAMESCOPECTRL_REQUEST_SCREENSHOT 1
  timeout 2 tail -f /dev/null
  cp /tmp/gamescope.png "$WORK/shot\$n.png" 2>/dev/null || true
done
EOF
  chmod +x "$WORK/probe.sh"
  WINEDEBUG=+ddraw,+d3d timeout -k 5 $((WARMUP + 25)) \
    gamescope --backend headless -w "$PROBE_W" -h "$PROBE_H" -- "$WORK/probe.sh" \
    > "$WORK/probe.log" 2>&1 || true
}

echo "probing $EXE at ${PROBE_W}x${PROBE_H} (headless)..." >&2
probe

# What the demo asked the display to be.
MODE=$(grep -oE 'SetDisplayMode iface [^,]*, width [0-9]+, height [0-9]+' "$WORK/probe.log" \
       | head -1 | grep -oE '[0-9]+, height [0-9]+' | tr -dc '0-9 ' | awk '{print $1"x"$2}')

# What it actually put pixels in: union of the shots, then trim the black border.
CONTENT=""
shopt -s nullglob
shots=("$WORK"/shot*.png)
if ((${#shots[@]})); then
  magick "${shots[@]}" -evaluate-sequence max "$WORK/union.png"
  CONTENT=$(magick "$WORK/union.png" -fuzz 2% -trim -format '%wx%h' info: 2>/dev/null || true)
fi

echo "  requested mode: ${MODE:-unknown}" >&2
echo "  drawn area:     ${CONTENT:-unknown}" >&2

RES=${CONTENT:-${MODE:-800x600}}
case $RES in *x*) : ;; *) RES=800x600 ;; esac
W=${RES%x*} H=${RES#*x}
(( W < 256 || H < 192 )) && { W=${MODE%x*}; H=${MODE#*x}; }   # trim went wrong, fall back

echo "running at ${W}x${H}" >&2
cd "$DIR"
exec gamescope -w "$W" -h "$H" -f -S fit -F nearest -- wine "$EXE"
```

## Applying it to demarc

In `src/wine_emu.rs`:

- `PICK_RES` stops being a guess. When `wine_res` is unset (and for
  `wine_res=pick` once the dialog has been answered), run the probe first and
  start the real session at what it reports.
- The probe costs one extra wine start. Against a warm prefix that is the cheap
  part of a launch — and since a relaunch restarts the demo from the top, which
  is what you want anyway, nothing is lost.
- `autodlg` cannot substitute for this. It runs *inside* the session, and the
  nested size is fixed once gamescope is up, so knowing the size from in there
  still requires the relaunch. What it can usefully add is the resolution the
  dialog was set to, as a hint that skips or seeds the probe.
- Keep `wine_desktop=true` for the demos that need it (Equinox's *Kings of the
  Playground*), but note it does not help with sizing — it fixes mode-switch
  survival, not scaling.
- `-S fit -F nearest` on the real run: `fit` letterboxes rather than stretching
  a 4:3 mode onto a 16:9 screen, `nearest` keeps the pixels hard.

## Useful commands

```sh
# what modes does a session advertise, and does a mode set work?
gamescope --backend headless -w 1024 -h 768 -- sh -c \
  'xrandr --current; xrandr --output gamescope --mode 640x480'

# what mode does a demo ask for?
WINEDEBUG=+ddraw gamescope --backend headless -w 1024 -h 768 -- wine demo.exe 2>&1 \
  | grep -m1 'SetDisplayMode iface'

# screenshot a running session (writes /tmp/gamescope.png)
xprop -root -f GAMESCOPECTRL_REQUEST_SCREENSHOT 32c \
  -set GAMESCOPECTRL_REQUEST_SCREENSHOT 1
```
