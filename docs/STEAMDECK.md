# demarc on the Steam Deck

Working notes from getting demarc running on a Steam Deck in Game Mode
(2026-10-09): what is installed where, what each workaround is for, what was
changed in the repo, and what is still open.

The Deck: SteamOS 3.6.20, glibc 2.39, glib 2.78, gamescope 3.15.14, bubblewrap
0.10.0, kernel 6.5. Reached as `deck@192.168.1.214` over ssh with a password.

---

## State

Working, confirmed on the device by the user:

- Emulated demos (C64, Amiga, Atari ST, ...) with video and audio.
- Browsing the Demozoo and CSDb databases with `--select`.
- Gamepad navigation in the picker and gamepad commands.
- Windows demos through wine and demarc's gamescope core.

Believed fixed, not confirmed by the user:

- Performance HUD flickering between two positions (see *IPC namespace*).
- Steam and `...` buttons dying (see *wine and the controller*). After the
  reboot Steam's log shows the Steam button registering at 18:55, after wine had
  run in the prefix at about 18:48, and the controller interface still has its
  kernel driver. Nobody has yet said "the button works during a Windows demo".

Open:

- When "1995" ended, demarc stayed on a black screen showing the title, with
  the inner gamescope a zombie child (`[gamescope-wl] <defunct>`). Not looked at.
- bubblewrap on the Deck is too old for demarc's wine sandbox (see *bwrap*).
- No Mono or Gecko in the prefix, so .NET demos will not run.
- Sticks do nothing; only the D-pad navigates. The pad is not passed to the
  emulated machine as a joystick.
- Nothing from this work is committed.

---

## What is on the Deck

```
~/demarc/
  demarc.sh            launcher, in Steam as the non-Steam game "demarc.sh"
  dm -> demarc-x86_64-unknown-linux-gnu/demarc    v1.8.0 release binary
  demarc-dev           stripped workstation build (what the launcher runs)
  glibc/               the workstation's ld-linux, libc, libm, libstdc++, ...
  demozoo.txt.gz csdb.txt.gz
  natrium.prg rebels.adf                     test files
  one.lua multi.lua hold.lua mon.sh gput.sh  test scripts, see "Testing"
  prefix-setup/        mk_wine_prefix.sh, files/, winetricks, setup.log
~/.local/share/demarc/wine/    Kron4ek wine 11.17 amd64-wow64, + cabextract in bin/
~/.wine-demarc/                the prefix
~/Demos/{1995,Lifeforce,Masagin}   Windows demos that were already there
```

Game Mode only shows windows that Steam launched. Anything started over ssh
plays audio behind the home screen, which is why there is a Steam shortcut.

### The launcher

`steamdeck/demarc.sh` in the repo, installed as `~/demarc/demarc.sh`.

demarc silences its own output, so `demarc.log` is normally empty; add
`--no-silence` to the arguments to get a log.

---

## Why each workaround is there

### Dev build on a borrowed glibc

A workstation build needs glibc 2.44; the Deck has 2.39. Only eight libm
symbols are newer (`acosf`, `asinf`, `atan2f`, `coshf`, `log10f`, `sinhf` at
2.43; `cosh`, `sinh` at 2.44). The launcher runs the binary through the
workstation's own loader with `--library-path`, which is not inherited by child
processes, so gamescope and wine use the Deck's libraries. The release binary
(`dm`) runs natively but has none of the changes below.

To update the dev build: `strip -o demarc-dev target/release-fast/demarc`, then
`scp` it over — while demarc is not running.

### `WINIT_X11_SCALE_FACTOR=1`

gamescope reports a 100x150 mm screen, winit guesses a scale factor of 2.17 from
it, and the window comes out 2773x1560 on a 1280x800 display.

### `DISABLE_GAMESCOPE_WSI=1`

SteamOS's gamescope 3.15 Vulkan layer is loaded into the demo (Steam sets
`ENABLE_GAMESCOPE_WSI=1`) and cannot talk to demarc's newer gamescope:
`[Gamescope WSI] Failed to get Wayland objects`, then
`Presenter: Failed to create Vulkan surface: VK_ERROR_SURFACE_LOST_KHR`, and a
black picture. This is launcher-only: the workstation has the same layer
installed and works with it.

### The `unset` lines

Steam injects its overlay and shader-cache Vulkan layers into everything it
launches. Both were found loaded inside demarc's own gamescope
(`steamoverlayvulkanlayer.so`, `libVkLayer_steam_fossilize.so` in its
`/proc/<pid>/maps`). The inner gamescope also inherited the outer one's
`GAMESCOPE_STATS` and `GAMESCOPE_LIMITER_FILE`. Whether these caused any visible
symptom was never isolated; they are unset on principle.

### IPC namespace (`bwrap --unshare-ipc`) — the HUD flicker

The performance HUD (mangoapp) reads frame statistics from a SysV message queue.
gamescope finds it with `ftok("mangoapp", 65)`, which fails and yields key
`0xffffffff` — the same for every gamescope on the machine. demarc's inner
gamescope and the Deck's own both sent to it, so the HUD alternated between two
sets of numbers and jumped between two positions.

`ipcs -q -p` shows the queue's last sender. During a demo it was demarc's
gamescope; inside a private IPC namespace it is only the Deck's (pid of the
session gamescope).

The private `/tmp/.X11-unix` is needed because bwrap's user namespace makes the
real directory appear owned by "nobody", and the inner Xwayland refuses a socket
directory it or root does not own. X0 and X1 are bound in so demarc still reaches
the Deck's displays.

The proper fix is in the gamescope fork: do not send mangoapp stats under the
libretro backend. `external/gamescope` is not checked out on the workstation, so
this was not done.

### `-x wine_res=1280x800`

Mean GPU load on the Deck, ten-second samples:

| Run | GPU |
|---|---|
| C64 demo, CRT shader | 42% |
| C64 demo, no shader | 16% |
| "1995" at 1280x720, CRT shader | 63% |
| "1995" at 1280x720, no shader | 58% |

At the 1920x1080 default the GPU sat at 99-100%. `wine_res` also sets the mode
demarc asks the demo's setup dialog for. The inner gamescope costs about half a
CPU core while a demo runs. The CRT filter stays on for emulated machines; only
Windows demos default to `wine_filter=false` on a Deck.

The session is the panel's own 1280x800, so a 4:3 demo can have 1024x768. A
demo cannot switch to a mode larger than the session: DXVK logs
`EnterFullscreenMode: Failed to change display mode` and the demo exits or
crashes, which looked like a black screen (Zoom 3 asking for 1024x768 in a
720-line session; Receptor, Wishful Twisting and Atrium started from their
`1920x1080` exe). demarc now drops such modes from `wine_dialog_res`, for the
setup dialog and for choosing between exes named after a resolution.

### Uncapped demos (`DXVK_CONFIG`)

With the WSI layer off, a D3D demo that does not ask for vsync presents
`VK_PRESENT_MODE_IMMEDIATE_KHR` and renders flat out while the session shows 60.
Zoom 3, 15-second samples through `perf.sh`:

| Run | FPS | GPU | GPU clock | demo CPU | demarc CPU |
|---|---|---|---|---|---|
| as is | 235 | 99% | 1533 MHz | 114% | 125% |
| `DXVK_CONFIG=d3d9.presentInterval=1` | 59.9 | 58% | 690 MHz | 29% | 93% |
| `DXVK_CONFIG=d3d9.maxFrameRate=60` | 59.9 | 41% | 732 MHz | 72% | 93% |

`DXVK_FRAME_RATE` is not read by this DXVK build. Panic Room (FIFO) and "1995"
(OpenGL) were at 60 already. `--max-threads 1` took demarc's own CPU from 77% to
69% of a core on a C64 demo and from 125% to 111% on Zoom 3; demarc still uses
about a core while showing a Windows demo, which has not been profiled.

The prefix is 400 MB smaller than the workstation's only because it has no
wine-mono; the rest matches file for file.

### WMA (`GST_PLUGIN_PATH`)

SteamOS's gstreamer has no ASF demuxer and no libav, so wine cannot open a
`.wma`. `libgstasf.so` (gst-plugins-ugly) and `libgstlibav.so` (gst-libav) from
the Arch archive, at the Deck's gstreamer version (1.22.10), sit in
`~/.local/share/demarc/gst`; the launcher points `GST_PLUGIN_PATH` there. They
link against the Deck's own ffmpeg 6.1.

Texas / Keyboarders needed that and an (empty) `C:\users\Public\Music\Sample
Music`, which `mk_wine_prefix.sh` now makes.

### Rupture

ASD's Rupture (zoo 60) played its music over a black picture, on any Mesa
driver. It takes ARB vertex programs only when `GL_VENDOR` contains "ATI" and
`NV_vertex_program` (Cg profile vp30) otherwise; Mesa says "AMD" and has no such
extension, so no vertex program loaded. `overrides.toml` now patches out the
vendor test.

### gamescope core: bundled `libgudev`

The launcher now moves it aside at every start.

The core's `lib/libgudev-1.0.so.0` needs glib 2.80 (`g_once_init_enter_pointer`);
the Deck has 2.78. It was moved to
`~/.cache/demarc/cores/<hash>/lib/libgudev-1.0.so.0.bundled` so the system copy
loads. A core update brings it back. `docs/GAMESCOPE.md` says the release needs
SteamOS 3.7; this is why. The real fix is in the core's release build.

### wine and the controller — the dead buttons

`wineusb.sys`, wine's USB passthrough driver, opens USB devices through libusb
and took the Deck's built-in controller. The controller is USB device `3-3`
(`28de:1205`); interface `3-3:1.2` is the one Steam reads. After wine had run,
that interface had no kernel driver, Steam's `logs/controller.txt` said
`Controller device closed after hid_read failure`, and every button was dead —
Steam and `...` included, in and out of demarc — until a reboot.

In the prefix now, and in `scripts/mk_wine_prefix.sh`:

```
HKCU\Software\Wine\DllOverrides   wineusb.sys = ""
HKLM\System\CurrentControlSet\Services\winebus   DisableHidraw=1  DisableInput=1  "Enable SDL"=0
```

The DLL override is the one that matters. Setting the `wineusb` service's
`Start` to 4 does not stop it loading. With the override a fresh `winedevice.exe`
has no `wineusb` mapped and holds no `/dev/bus/usb` descriptors (it held six
before). The winebus values were an earlier, wrong guess at the cause; they keep
wine off `/dev/hidraw*` and are harmless, but demos get no gamepad inside wine.

To check:

```sh
ls -l /sys/bus/usb/devices/3-3:1.2/driver                 # must be usbhid
ls -l /proc/$(pgrep -x steam | head -1)/fd | grep hidraw  # Steam must hold some
grep "hid_read failure" ~/.local/share/Steam/logs/controller.txt
ls -l /proc/<winedevice pid>/fd | grep bus/usb            # must be empty
```

A `winedevice.exe` once survived both killing demarc and `wineserver -k`, and
needed `pkill -9 winedevice.exe`.

### bwrap

demarc's wine sandbox uses `--overlay-src` / `--tmp-overlay`, which bubblewrap
0.10.0 does not have. Plain user namespaces work. demarc logs the misleading
"unprivileged user namespaces or overlayfs unavailable" and falls back to the
one shared prefix, one demo at a time. A newer `bwrap` in
`~/.local/share/demarc/wine/bin` should be picked up; untried, and the kernel
would still have to allow an overlay mount inside a user namespace.

### Two gamescopes

Nothing has to be turned off. demarc's gamescope composites into a shared buffer,
takes wayland display `gamescope-1` and Xwayland `:2`, and runs beside the
Deck's. The things that did collide were the WSI layer, the mangoapp queue and
the inherited environment, all covered above.

---

## Repo changes (uncommitted)

- `src/commands.rs` — gamepad support. `PAD_HOTKEYS` maps a button, alone or
  with a held modifier, to a `Cmd` (`PadMapping::new` / `PadMapping::with`); a
  plain mapping does not fire while a modifier button is held. `PAD_NAV` turns
  D-pad, L1/R1, A and X into arrow, page, Enter and Escape key messages while a
  picker or dialog is open, with key repeat. Bevy's gamepad support was already
  compiled in.

  | Button | Command | | Button | In a dialog |
  |---|---|---|---|---|
  | X | Open file menu | | D-pad | arrows |
  | Y | Toggle info | | L1 / R1 | Page Up / Down |
  | R1 / L1 | Next / previous file | | A | Enter |
  | Start | Pause/resume | | X | Escape |
  | B | Open on-screen keyboard | | | |
  | L2+Y | Toggle CRT filter | | | |
  | L2+X | Change scale | | | |
  | L2+R1 | Warp 10s | | | |
  | L2 (tap) | Command list | | | |

- `src/remote_control.rs` — `key_message` is `pub(crate)`.
- `src/tests/commands_tests.rs` — a test for the modifier rule.
- `crates/newsys/src/wine.rs`, `crates/newsys/src/newsys/windows.rs` —
  `fitting_modes` keeps modes larger than `wine_res` out of `wine_dialog_res`.
- `crates/newsys/src/wine.rs`, `src/main.rs` — `add_wine_to_path` puts
  `~/.local/share/demarc/wine/bin` first on `PATH` at startup when it exists.
- `scripts/mk_wine_prefix.sh` — makes the downloaded `winetricks` executable,
  fixes `wcho`, and adds the wineusb and winebus registry lines.
- `docs/LEARNINGS.md` — two entries.

---

## Setting it up again

`steamdeck/package.sh`, run on the workstation, builds
`target/steamdeck/demarc-steamdeck.zip`: `demarc.tar.gz` plus `install.sh`
(macOS/Linux) and `install.bat` (Windows), which need only `ssh` and `scp`.
`install.sh deck@<address>` copies the tarball to the Deck, unpacks it into
`~/demarc` and runs `setup.sh` there, which does all of the below and skips what
is already there; it is also how a new build is pushed. The Deck needs sshd on
and a password set, which takes one visit to Desktop Mode (`passwd`, `sudo
systemctl enable --now sshd`).

Before the split into these scripts, only the copy, the skips and `--check-wine`
had been run; the wine download and the prefix step have not, and neither has
`install.bat`.

1. Unpack a wow64 wine build into `~/.local/share/demarc/wine` (Kron4ek
   `wine-11.17-amd64-wow64`; 11.16 has a wineserver double free). Copy the
   workstation's `/usr/bin/cabextract` into its `bin/` — SteamOS has none.
2. Copy `scripts/mk_wine_prefix.sh` and `files/` into one directory and run it
   with that `bin` on `PATH`, `WINEDLLOVERRIDES="mscoree=d;mshtml=d"` (or
   `wineboot` waits on a Mono/Gecko prompt) and a `DISPLAY`. **Do it with nothing
   running on the Deck**, and expect to reboot afterwards: the script's first
   `wineboot` runs before the wineusb override is written, so it can still take
   the controller. Moving the override to right after `wineboot -i` would not
   help for the same reason; setting `WINEDLLOVERRIDES=wineusb.sys=d` for the
   whole script should, and is untried.
3. `demarc --check-wine` should list wine, bwrap and the prefix as ok.
4. `steamos-add-to-steam ~/demarc/demarc.sh` (with `DISPLAY=:0`).

---

## Testing from the workstation

ssh: there is no `sshpass`. Use an askpass script and a control socket on a
short path:

```sh
SSH_ASKPASS=<script echoing the password> SSH_ASKPASS_REQUIRE=force setsid -w \
  ssh -o ControlMaster=auto -o ControlPath=/tmp/claude-1000/deck-cm \
      -o ControlPersist=2h deck@192.168.1.214 true
```

- **Headless** (no window, no audio, safe while the Deck is in use, but shares
  the wine prefix): `./glibc/ld-linux-x86-64.so.2 --library-path ./glibc:/usr/lib
  ./demarc-dev <file> --headless --no-silence --remote-control multi.lua`.
  `multi.lua` saves five screenshots 300 frames apart; a single early screenshot
  is often black because the machine is still booting.
- **On the real screen:** write the arguments to `~/demarc/test-args`, then
  `DISPLAY=:0 steam steam://rungameid/9770677391545335808`. Remove `test-args`
  afterwards. `hold.lua` keeps demarc up for 2700 frames and quits.
- **Seeing the screen:** `DISPLAY=:0 xprop -root -f
  GAMESCOPECTRL_DEBUG_REQUEST_SCREENSHOT 32c -set
  GAMESCOPECTRL_DEBUG_REQUEST_SCREENSHOT 1` writes `/tmp/gamescope.png`. It does
  not include overlays such as the performance HUD.
- **Stopping demarc:** `pkill -x ld-linux-x86-64`. Never `pkill -f` or
  `pgrep -f` over ssh: the pattern matches the remote shell's own command line.
- `perf.sh <label> <warmup s> "<ENV=...>" <args...>` runs demarc through Steam and
  prints mean GPU load and clock and the CPU of demarc, its gamescope and the
  demo; the launcher sources `test-env` for it. `hold.lua` ends a run after 45 s.
- `mon.sh <seconds>` samples GPU load, hidraw holders, focus and top CPU users;
  `gput.sh <label> <args...>` runs demarc through Steam and prints mean GPU load.
- `DISPLAY=:1` is the display the running game is on.
- Steam's logs are in `~/.local/share/Steam/logs/`: `controller.txt`,
  `controller_ui.txt` ("Guide button sent to JS" is the Steam button),
  `gameprocess_log.txt` (launches and exits of the shortcut, AppID 2274913106).
