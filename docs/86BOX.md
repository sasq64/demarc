# 86Box as a libretro core

`external/86box/` is a clone of <https://github.com/86Box/86Box> carrying the
same kind of patch as `external/pcem/`: a `-DLIBRETRO=ON` build that produces
`86box_libretro.so` instead of an application. 86Box is a fork of PCem and is
considerably more accurate on the early machines — accurate enough to run
8088 MPH, which PCem cannot.

See `docs/PCEM.md` for the PCem core; most of what it says about a PC config as
content, ROM placement and the absence of savestates applies here too.

---

## Quick start

```sh
cmake -S external/86box -B external/86box/build-lr -DLIBRETRO=ON \
    -DCMAKE_BUILD_TYPE=Release -DRTMIDI=OFF -DFLUIDSYNTH=OFF -DMUNT=OFF -DSOUNDCANVAS=OFF
cmake --build external/86box/build-lr -j

DEMARC_CORE_DIR=external/86box/build-lr/src ./target/release-fast/demarc machine.cfg
```

BIOS ROMs are **not** shipped. 86Box wants its own layout, not PCem's: put them
under `<system dir>/86box/roms/`, as `roms/machines/<machine>/<file>` and
`roms/video/...`, with exactly the file names `src/machine/*.c` asks for. The
log the core prints on startup lists every path it searched.

`scripts/libretro-smoke.py` drives the core with no Bevy in the way, which is
the quickest way to tell a core problem from a frontend one.

---

## What the patch is

The emulator itself needed one change — unlike PCem, where `fatal()`, the
config's relative paths and the CMOS fallback all had to be touched. 86Box
already splits its frontends into two object libraries, `plat` and `ui`, so
`src/libretro/` is mostly a third implementation of them:

| File | Contents |
|---|---|
| `lr_core.c` | `retro_*` entry points, options, the `retro_run` loop |
| `lr_video.c` | blit sink, geometry, `startblit`/`endblit` |
| `lr_sound.c` | `givealbuffer_common()` over a mixing ring, replacing `sound/openal.c` |
| `lr_input.c` | `RETROK_*` → XT scancode table, relative mouse |
| `lr_joystick.c` | two `RETRO_DEVICE_JOYPAD`s as gameport sticks |
| `lr_ui.c` | status bar / message box / window title stubs |
| `lr_plat*.c` | `unix/sdl_plat*.c` with the handful of SDL calls replaced |

Three things differ from the PCem core and are worth knowing:

- **Pacing.** `pc_run()` is one millisecond of emulated time (ten with
  `force_10ms`), against PCem's fixed ten, so `retro_run` drives it from a
  millisecond accumulator: ~17 calls per tick at 60 fps.
- **The blit is on a thread.** 86Box's `video.c` hands each blit to a
  per-monitor blit thread, which calls whatever `video_setblit()` registered.
  The sink latches the frame under a mutex and `retro_run` presents it at the
  tick boundary.
- **Audio sources are many and at different rates** — the OPL at 49716 Hz, CD
  at 44100, the main mix at 48000, each from its own thread. OpenAL gave every
  source its own AL source and let the mixer line them up; here each keeps an
  absolute position in output frames and is resampled onto one accumulator ring
  that `retro_run` drains.

## Content

A machine `.cfg`, the same file the desktop 86Box writes. It goes to `pc_init()`
as `-C`, with `-P` pointed at the directory the `.cfg` sits in, so the images it
names resolve beside it and a machine is one relocatable directory —
`config.c` already resolves relative image paths against `usr_path`, which is
the patch PCem needed and 86Box did not.

`src/newsys/dos.rs` tells the two emulators' configs apart by the key that names
the machine: `machine =` is 86Box, `model =` is PCem.

The one emulator change is `config_readonly`, which `config_save()` honours and
the core sets. Content is not ours to write to, and loading a hand-written
`.cfg` is enough to set `config_changed` and have `pc_reset_hard_init()` save a
normalised copy back over it — comments and all.

## 8088 MPH

`8088MPH/8088mph-86box.cfg` is an IBM XT (1986) with 640K, an 8088 at 4.77 MHz
and CGA on a composite monitor, booting MS-DOS 6.22 off A: with the release's
own diskette in B:. It runs: POST, DOS, the title screen, the 1024-colour and
256-colour parts, with PC speaker audio.

## Area 5150

`area5150/area5150-86box.cfg` is the same XT with a 20MB ST-225 on the IBM
fixed disk adapter (`hdc_1 = st506_xt`, 17/4/615 MFM), which is what the
release wants: it boots off the same MS-DOS floppy in A: and runs
`C:\AREA5150\AREA5150` from `area5150.img`. The adapter's BIOS is not shipped
either — `roms/hdd/st506/ibm_xebec_62x0822_1985.bin`, the same file PCem uses.

## Open work

- Only the primary monitor is presented; a second head is blitted but dropped.
- Aspect is fixed at 4:3, as in the PCem core.
- No savestates, no `SET_DISK_CONTROL_INTERFACE`.
- The core is built from a local checkout; the libretro buildbot does not ship
  86Box, so there is no download story yet — same as PCem.
