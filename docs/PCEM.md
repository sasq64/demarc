# PCem as a libretro core

demarc drives every emulated system through a libretro core, and PCem shipped
only wxWidgets and Qt GUIs. `external/pcem/` now carries a third display engine,
`PCEM_DISPLAY_ENGINE=libretro`, that builds `pcem_libretro.so` and gives demarc
PC/DOS support.

This file is the working reference for that work: what exists, how to run it,
what was learned about PCem's internals along the way, and what is still open.

---

## Quick start

```sh
just pcem-core                      # builds external/pcem/build-lr/src/pcem_libretro.so
just pc testdata/pc/ibmxt.cfg       # runs it through demarc
just pc testdata/pc/2ndreality.cfg  # a 486 booting DOS and running a demo
```

BIOS ROMs are **not** shipped — they are copyrighted. Put a PCem ROM set under
`<system dir>/pcem/roms/` (that is `./system/pcem/roms/` in a debug build,
`~/.cache/demarc/system/pcem/roms/` otherwise). `external/pcem/docs/roms.txt`
lists what each machine needs.

The end-to-end test, which needs both the core and the ROMs:

```sh
DEMARC_CORE_DIR=external/pcem/build-lr/src \
    cargo test boots_an_ibm_xt -- --ignored --nocapture
```

It boots a real IBM PC/XT to Cassette BASIC in about six seconds:

```
POST: 640 KB OK
booted to ROM BASIC at frame 2228:
The IBM Personal Computer Basic
Version C1.10 Copyright IBM Corp 1981
62940 Bytes free
Ok
```

The 486 one is `runs_second_reality_from_a_dos_hard_disc`, same invocation with
`cargo test runs_second_reality`. It is **not passing yet** — see "Booting DOS"
below for what works and what is left.

`scripts/libretro-smoke.py` does the same job for any core, from Python, with no
Bevy in the way — useful when a failure could be the core, the threading, or the
frontend and you need to tell them apart. It cannot press keys, so anything past
a boot prompt needs the Rust test.

---

## Where the code is

### In `external/pcem` (that repo's own git, branch `dev`)

New, ~1500 lines excluding the vendored `libretro.h`:

| File | Contents |
|---|---|
| `src/libretro-ui/libretro-core.c` | `retro_*` entry points, options, the `retro_run` loop |
| `src/libretro-ui/libretro-video.c` | `create_bitmap`/`hline`/`screen`, blit sink, geometry |
| `src/libretro-ui/libretro-sound.c` | `givealbuffer` over a ring buffer |
| `src/libretro-ui/libretro-keyboard.c` | `RETROK_*` → XT scancode table |
| `src/libretro-ui/libretro-mouse.c` | relative motion, buttons |
| `src/libretro-ui/libretro-joystick.c` | two 2-axis/4-button sticks |
| `src/libretro-ui/libretro-thread.c` | copied verbatim from `wx-ui/wx-thread.c` |
| `src/libretro-ui/libretro-viewers.c` | no-op debug viewers |
| `src/plugin-api/libretro-utils.c` | the three `wx_*` host helpers core code calls |
| `includes/private/libretro-ui/` | `ui-utils.h`, `viewer.h`, `viewer_voodoo.h` |

Modified upstream files, ~150 lines total: `CMakeLists.txt`, `src/CMakeLists.txt`,
`includes/includes.cmake`, `cmake/install.cmake`, `src/sound/sound.cmake`,
`src/plugin-api/plugin-api.cmake`, `src/plugin-api/paths.c`,
`src/plugin-api/logging.c`, `includes/private/plugin-api/plugin.h`,
`src/cpu/x86seg.c`, `src/pc.c`, `src/devices/nvr.c`.

The last two are both `#ifdef PCEM_LIBRETRO` and both exist so a `.cfg` plus its
images and CMOS can be one relocatable directory:

- `resolve_config_paths()` in `pc.c`, called straight after `loadconfig()`,
  rewrites relative `disc_a`/`disc_b`/`hd?_fn`/`cdrom_path` to be relative to the
  `.cfg`. Upstream never needed it because PCem's GUIs only write absolute paths.
- `nvrfopen()` in `devices/nvr.c` gains a third read fallback, after
  `<nvr>/<config>.<name>` and `<nvr>/default/<name>`: `<config dir>/<name>`.
  Writes still go to the save directory, so the copy in testdata is read-only.

### In demarc

- `src/newsys/pc.rs` — `PcSystem`, the config sniffer, the screen decoder, the boot test
- `src/libloader.rs` — `$DEMARC_CORE_DIR` override
- `build.rs` — `SKIP_DIRS`, so `system/pcem/` never enters `system.zip`
- `testdata/pc/ibmxt.cfg`, `scripts/libretro-smoke.py`, `Justfile`, `.gitignore`
- `testdata/pc/2ndreality.cfg` + `fdboot.img`, `2ndreality.img`, `ami486.nvr`
- `scripts/make-pc-disks.sh` and `scripts/make-pc-cmos.py`, which regenerate them

---

## What PCem's platform seam actually is

The headline finding: **the emulation core is toolkit-free.** Grepping SDL,
OpenGL, wxWidgets, Qt and OpenAL across every non-UI source returns exactly two
hits — `SDL_GetBasePath` in `plugin-api/paths.c`, and OpenAL in
`sound/soundopenal.c`. Both were trivially replaceable. And because `wx-ui/` and
`qt-ui/` are near-identical mirrors of each other, the core↔frontend contract
can be read off by diffing them.

The whole contract is about 30 functions:

| Header | Functions |
|---|---|
| `thread.h` | 12 × `thread_*` |
| `ibm.h` | `startblit`, `endblit`, `set_window_title`, `updatewindowsize`, `timer_read` + `timer_freq`, `stop_emulation_now` |
| `video/video.h` | `create_bitmap`, `destroy_bitmap`, `hline`, `VIDEO_BITMAP *screen`, `video_blit_memtoscreen_func` |
| `plat-keyboard.h` | `keyboard_init/close/poll_host`, `pcem_key[272]`, `rawinputkey[272]` |
| `plat-mouse.h` | `mouse_init/close/poll_host`, `mouse_get_mickeys`, `mouse_buttons`, `mousecapture` |
| `plat-joystick.h` | `joystick_init/close/poll`, `plat_joystick_state[8]`, `joystick_state[4]` |
| `sound/sound.h` | `initalmain`, `inital`, `givealbuffer`, `givealbuffer_cd`, `sound_buf_len_al` |
| `plat-midi.h` | 5 × `midi_*` |
| `wx-ui/viewer.h` | `viewer_*` + five `viewer_t` globals |
| `ui-utils.h` | **only three** reach core code: `wx_dir_exists`, `wx_get_home_directory`, `wx_create_directory` |

Things the GUIs define that the core never calls, and a new frontend can skip:
`get_ticks`, `delay_ms`, `getfile`, `getsfile`, `deviceconfig_open`,
`pause_emulation`, `screenshot_taken`, the fullscreen calls, and roughly seventy
other `wx_*` dialog/menu helpers.

Two ready-made pieces are already in the tree:

- **`src/wx-ui/wx-thread.c` is pure Win32/pthread** despite its name — no SDL, no
  wx. Copied verbatim.
- **`src/sound/sdl2-midi.c` includes no SDL** — it is five empty functions, which
  is exactly right for a core with no host MIDI ports.

Do **not** start from `src/thread-pthread.c`. It looks like the obvious base but
is dead code in no CMake list, its `thread_reset_event()` is a no-op, and it has
no mutex functions at all. `video.c`'s blit handshake and `sound.c`'s CD thread
both rely on manual-reset event semantics and would deadlock.

---

## Design decisions

### Timing: pace `runpc()`, never resize it

`runpc()` (`src/pc.c:468`) runs `cpu_get_speed() / 100` cycles — exactly 10 ms of
emulated time. That 100 Hz cadence is baked into `keyboard_process()`'s typematic
repeat (`keydelay -= 10`) and `pollmouse()`'s `pollmouse_delay = 2`, so shortening
it would break key repeat and halve the mouse rate.

`retro_run` therefore keeps `runpc()` intact and drives it from a millisecond
accumulator: 1 or 2 calls per frame at 60 fps. The audio ring absorbs the
resulting burstiness. The wall-clock `drawits` throttle from `mainthread()` is
deleted outright — the frontend paces us, which is a straight improvement over
the SDL frontend.

`pcem_fps` switches between 60 and 100. At 100 it is one `runpc()` per tick,
preserving PCem's original timing exactly, at the cost of a 100 Hz core.

Measured: 120 ticks at 60 fps produce exactly 96000 audio frames; 100 ticks at
100 fps produce exactly 48000.

### Video: latch the frame, take geometry from the blit

Frames are **not** produced by `runpc()`. Video cards emit them from their own
PC-timer callbacks *inside* `exec386()`, so a frame can land part way through
`retro_run`. The blit sink copies into `screen` and sets a flag;
`lr_video_present()` hands it over at the end of the tick, or passes `NULL` for a
dupe when the emulated refresh did not line up with ours.

No pixel conversion is needed: `makecol()` in `video.h` is
`b | (g << 8) | (r << 16)`, which *is* `RETRO_PIXEL_FORMAT_XRGB8888` on
little-endian.

`updatewindowsize()` is deliberately ignored. It is a *window size* hint — CGA
passes `(ysize << 1) + 16` to ask the GUI for line doubling — not geometry in the
libretro sense. Reporting it caused the core to advertise 416 rows while
delivering 208. `SET_GEOMETRY` now follows the blit dimensions, with a fixed 4:3
aspect, which is the monitor these machines actually drove and handles doubling
for free.

### Audio: a ring, because `givealbuffer` runs mid-`retro_run`

`sound_poll()` is a PC timer at 48 kHz; every `sound_buf_len_al` frames it mixes
the cards and calls `givealbuffer()` — from inside `exec*()`. It must not block.
The OpenAL sink silently dropped the buffer when no AL buffer was free; the
replacement pushes into a ring that `retro_run` drains at a fixed
`48000 / fps` frames per tick.

`sound_buf_len` defaults to 200 ms (2400-frame callbacks), far too coarse for
60 fps. The `pcem_sound_buf_len` option sets it to 20 ms, so several callbacks
land per frame. Note `sound.c:125` initialises it to `48000/10`, which would make
`sound_update_buf_length()` divide by zero if config never loaded.

`givealbuffer_cd` (44.1 kHz, from a real worker thread) is dropped for now — it
only matters for Red Book audio off a CD image, and mixing it means resampling
across a thread boundary.

### Content: a `.cfg`, and nothing else

`initpc()` already parses `--config`, so `retro_load_game` synthesises
`{"pcem", "--config", path}` and passes the content path straight through. The
machine, CPU, video card, sound card and mounted images all come from that file.

`.cfg` is far too generic an extension to accept on its own, so `PcSystem`
requires a `model =` key — see `is_pcem_config` in `src/newsys/pc.rs`.

Core options cover only what a machine config should not: `pcem_fps`,
`pcem_dynarec`, `pcem_sound_buf_len`. They are **legacy v0**
`SET_VARIABLES` strings on purpose — demarc answers `GET_CORE_OPTIONS_VERSION`
with 0, so a v2 option array is rejected outright.

### Paths, and keeping ROMs out of `system.zip`

`paths.c`'s globals are all runtime-settable, so ROMs come from
`<system dir>/pcem/roms/` and everything writable (NVR, logs, configs) goes under
`<save dir>/pcem/`. `get_pcem_path()` — the core's only SDL call — is behind
`PCEM_LIBRETRO`.

demarc returns **the same path** for `GET_SYSTEM_DIRECTORY` and
`GET_SAVE_DIRECTORY`, and `build.rs` packs the whole `system/` tree into the
binary. In a debug build `system_dir()` *is* the repo's `system/`, so without
care a running machine would fill the archive with NVR — and 25 MB of
copyrighted BIOS dumps would be committed. Hence `SKIP_DIRS` in `build.rs` and
`/system/pcem/` in `.gitignore`. This is the same trap Beetle PSX's `.mcd` files
already hit (`pcsx-card2.mcd` in `MARKER_FILES`).

### `exit()` in a core is a bug

A core lives in the frontend's address space, so `exit(-1)` takes the whole
application down. `plugin-api/logging.c` gained a `_fatal_hook`; the core's hook
requests `SHUTDOWN` and `longjmp`s back out to `retro_run`, which then refuses to
run anything further until `retro_reset`.

Checking the object files rather than grepping the sources is the reliable way to
find these — only **three** compiled objects reference `exit`/`abort`:

```sh
for o in $(find build-lr -name '*.o'); do
    nm -u "$o" | grep -qE '^ +U (exit|abort)$' && basename "$o"
done
```

- `logging.c.o` — `fatal()`, hooked
- `x86seg.c.o` — `x86abort()`, hooked; the CPU's "impossible state" bail
- `pc.c.o` — the `--help` path, unreachable from our argv

The ~90 other `exit(` hits in `grep` are all inside commented-out debug blocks.
In particular **`src/codegen/codegen_x86-64.c` is dead code** — not in any CMake
list, only its header is — so the `exit(-1)` in it never runs. The live allocator
is `codegen_allocator.c`, which contains no `exit` at all.

A `fatal()` on one of the video cards' worker threads still cannot be unwound
(`longjmp` across threads is undefined) and falls through to `exit`. That is a
real remaining gap, noted in the code.

---

## Booting DOS: the `.cfg` is not enough

The XT config works because an XT with no disks falls through to ROM BASIC. Any
AT-class machine that has to *boot* something needs four more things, and each
one of them was a separate dead end before it was found. All of this is baked
into `testdata/pc/2ndreality.cfg` and the two scripts that build its artefacts.

### CMOS, or the machine never boots at all

A 486 BIOS keeps the whole machine description — floppy types, hard disc
geometry, display type, memory size — in battery-backed CMOS, and PCem starts
every new machine with that empty. The AMI BIOS then stops at `RUN SETUP
UTILITY / Press <F1> to RESUME` and never looks at a disc. So content that is
meant to boot unattended has to ship its CMOS, which is what the `nvrfopen`
fallback above is for: `testdata/pc/ami486.nvr` sits next to the `.cfg`.

`scripts/make-pc-cmos.py` writes it. Two hard-won details:

- **Start from PCem's own `nvr/ami486.nvr`**, don't build 128 bytes from
  scratch. A from-scratch image with a textbook-correct standard-AT layout —
  including a checksum over 0x10..0x2D, which is provably the range PCem's file
  uses — still gets `CMOS checksum failure`. Something in AMI's private bytes
  matters and it was not worth finding out what. Patching the shipped file and
  recomputing that same checksum works.
- **Bit 5 of CMOS 0x2D is the boot sequence**, and PCem's default has it clear,
  meaning C: first. With a data-only hard disc that means the BIOS loads a zeroed
  MBR, jumps into it and hangs — POST completes, the configuration table paints,
  and then nothing, which looks nothing like a boot-order problem. Setting the
  bit gives A:,C: and everything works.

The geometry in the CMOS, the geometry in the `.cfg` (`hdc_cylinders` and
friends) and the geometry the image was built with all have to agree; PCem takes
its geometry from the config, not from the image.

### Discs, because PCem has no host-directory mount

Unlike DOSBox there is no way to hand DOS a folder, so `scripts/make-pc-disks.sh`
builds two images with mtools:

- `boot` makes a FreeDOS 1.3 boot floppy. The one part that cannot be built from
  nothing is a FAT12 boot sector that knows how to load `KERNEL.SYS`, so it is
  transplanted from the official `720k/x86BOOT.img` — jump and OEM name at 0x00,
  boot code at 0x3E, with mformat's own BPB left in between. Make it **1.44M**:
  a 720K image with drive A: declared as 2.88M in CMOS does not read.
- `data` makes a partitioned FAT hard disc from a directory. `mpartition -I`
  then `-c -a`; the active flag matters.

The floppy boots to `C:\` and runs `C:\AUTORUN.BAT` if there is one, so it stays
content-agnostic and every PC config in testdata can share the one image.

### Conventional memory

FreeDOS with a plain `SHELL=` line leaves ~506K free, and DOS-era demos commonly
want more — Second Reality refuses to start under 570,000 bytes. `JEMMEX` plus
`DOS=HIGH,UMB` and `SHELLHIGH=` gets it over the line, and provides the EMS that
a Sound Blaster mixdown wants on top.

### Sound card selection is the open blocker

Second Reality opens on its own SETUP screen and defaults to a Gravis
Ultrasound. If the card is not there it prints `Failed to initialize the selected
soundcard` and drops back to DOS — which is exactly what the run below shows,
and the reason the test does not pass yet. `gus = 1` is set in the config, so
PCem is emulating one; the demo presumably wants `ULTRASND` in the environment,
or the test has to walk the menu (the cycle is GUS, No sound, SoundBlaster,
SoundBlaster Pro) with `->` before pressing Enter.

---

## Testing

`boots_an_ibm_xt_to_rom_basic` in `src/newsys/pc.rs` drives the real demarc path
— `PcSystem` sniffs the config, `RetroCoreThreaded` runs the core on its worker
thread — and **asserts on text, not pixels**. A frame hash would say the picture
changed, not that the machine booted, and would go stale on any cosmetic PCem
change.

The `screen` module decodes the CGA framebuffer back into 80×25 characters. Two
things make that work:

- The 8×8 font is the **fourth** 2048-byte block of `mda.rom`. PCem's
  `loadfont(.., FONT_MDA)` reads that file as four blocks — two halves of the
  8×14 MDA font, then the *thin* and the *thick* 8×8 CGA fonts — and text modes
  render with the last one.
- The text area starts 8 pixels in and 4 down, because the card blits its
  overscan border too (80×25 is 640×200, the blit is 656×208).

Cells are matched as-is and inverted, so the black-on-white function key bar
reads like everything else. It matched 2000/2000 cells on the boot screen.

Two traps worth remembering:

- **`Backend::run()` is a non-blocking `try_recv`.** `false` means "no frame
  ready yet", not "no more frames". A loop that treats it as a step will spin
  through thousands of iterations in milliseconds and see nothing.
- **Do not break on the banner.** BASIC prints it a character at a time, so
  sampling mid-print catches `Copyright IBM Corp 198?`. `Ok` — the interpreter's
  prompt — is the honest end-of-boot marker.

`runs_second_reality_from_a_dos_hard_disc` is the 486 counterpart, and cannot
read text back: the ET4000 renders 80x25 with a 9x16 font out of its own BIOS,
not the 8x8 one in `mda.rom`. It asserts on the *shape* of the run instead —
leave text mode for the demo's own mode, then keep changing — which still cannot
happen unless the BIOS, the CMOS, DOS, the hard disc and the video card all work.
It presses Enter once a second rather than trying to recognise the SETUP screen;
the presses that land at the DOS prompt do nothing.

The mode trace it prints is the useful part when it fails. A good run so far:

```
656x200 at 1, 80x200 at 16, 80x400 at 18, 720x400 at 32, 80x400 at 160,
720x400 at 167, 80x400 at 677, 640x400 at 684, 80x400 at 8381, 640x400 at 8387
```

- `656x200` is PCem before any card has set a mode, so "not 80x25" is not a
  usable test for "the demo started" — wait for a text mode first.
- `720x400` at 32 is the ET4000 BIOS, `640x400` at 684 is the demo's SETUP
  screen (8x16 cells, no 9-dot clock), which is where Enter is being accepted.
- The second `640x400` at 8387 is the demo falling back to DOS after the sound
  card failure. There is no 320x200 anywhere, which is the whole problem.

The test also asserts the POST prints `640 KB OK`, which is the one number the
config's `mem_size` and the emulated hardware have to agree on before the machine
will come up at all.

---

## Known limitations

- **No savestates, ever.** PCem has no serialisation anywhere; every `device_t`
  keeps opaque `void *p` state across ~100 machines, ~50 video cards and ~20
  sound cards. `retro_serialize_size()` returns 0, so no rewind, netplay or
  runahead.
- **Not truly single-threaded.** Voodoo (1 FIFO + up to 4 render threads), S3,
  ViRGE, Mystique, ET4000W32, TGUI9440 and PGC each spawn workers that the card
  emulation blocks on. `retro_run` is single-threaded from the frontend's point
  of view, which is normal for a core; making them synchronous would be a large
  and risky rewrite.
- **One instance per process.** All state is file-scope globals — `ram`,
  `cpu_state`, `pit`, `buffer32`, `pcem_key`, the paths buffers, the device
  registries. demarc's per-instance temp-copy of the `.so` makes two instances
  survivable.
- **Dynarec covers x86, x86-64, arm32 and arm64 only**; any other host fails to
  link. JIT pages are `PROT_READ|WRITE|EXEC` anonymous mmap, which is disallowed
  on iOS/tvOS and unfriendly to hardened Linux.
- **GPLv2.** The core is GPLv2 like PCem itself. demarc `dlopen`s it exactly as
  it already does VICE and PUAE, so nothing changes.

---

## Open work

1. **Get Second Reality past its SETUP screen** — pick a sound card the machine
   actually has, either by setting `ULTRASND` in `AUTORUN.BAT` or by walking the
   menu from the test. That finishes the AT-class coverage: the 486 config
   already exercises the 486 core, the CMOS path, IDE and the ET4000.
2. **CD audio.** `givealbuffer_cd` is dropped; wiring it up needs a 44.1 → 48 kHz
   resample across a thread boundary.
3. **Networking.** `USE_NETWORKING` defaults off for this engine to avoid the
   hard `slirp` and `libpcap` finds. Turning it on should just work.
4. **Distribution.** `PCEM_MARCH` still defaults to `x86-64-v2`; a shipped core
   needs a conservative baseline. There is also no story yet for *getting* the
   core to users — demarc downloads every other core from the libretro buildbot,
   which does not build PCem.
5. **Aspect ratio.** Fixed 4:3. Right for essentially every PC mode, arguably
   wrong for 1280×1024. Could become a core option.
6. **Worker-thread `fatal()`.** Still calls `exit()`; see above.
7. **Disk swapping.** No `SET_DISK_CONTROL_INTERFACE`, so demarc's disk-swap UI
   does nothing. Floppies come from the `.cfg` only.
8. **`testdata/pc/2ndreality/`** still holds the loose demo files that
   `2ndreality.img` was built from, so they exist twice. Keep the directory as
   the script's input and leave it untracked, or commit only the image.
9. **Logging is compiled out of a release core.** `pclog` is behind
   `#ifndef RELEASE_BUILD`, so `just pcem-core` produces a core that says
   nothing. Configure a second build tree with `-DCMAKE_BUILD_TYPE=Debug` before
   trying to work out why a machine will not boot.
