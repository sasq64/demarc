# ZEsarUX as a libretro core

`external/zesarux/` is a clone of <https://github.com/chernandezba/zesarux>
on a `libretro` branch, which builds `zesarux_libretro.so`. It is there for the
ZX Spectrum Next (`--machine tbblue`): no buildbot core emulates the Next.

---

## Quick start

```sh
just zesarux-core
just next demo.nex
```

`zesarux-core` is `make -f Makefile.libretro` in `external/zesarux/src`. No
`./configure` is needed: `src/libretro/compileoptions.h` is force included
in place of the generated one, and the objects go to `src/libretro/obj`.
Without `DEMARC_CORE_DIR` the core is fetched from
`github.com/sasq64/zesarux` releases, like the other non-buildbot cores.

`system/zesarux/` holds the two ROMs the core asks for, `48.rom` and
`tbblue_loader.rom`, which ship with ZEsarUX. A `.nex` replaces the whole
machine state, so it plays even without them. `tbblue.mmc`, the 64 MB SD
image, is only passed to the core when `<system dir>/zesarux/tbblue.mmc`
exists. demarc does not ship it.

`scripts/libretro-smoke.py external/zesarux/src/zesarux_libretro.so demo.nex
--system-dir <system dir>` runs the core with no Bevy involved.

In demarc, `SpectrumNextSystem` (`crates/newsys/src/newsys/spectrum_next.rs`)
claims `.nex` and `.snx` by extension. No database has a Next platform:
pouet files it under "ZX Enhanced", together with TS-Conf and other clones.

---

## What the patch is

| File | Contents |
|---|---|
| `src/libretro/lr_core.c` | `retro_*` entry points, the command line handed to `zesarux_main()`, the frame loop and input |
| `src/libretro/lr_drivers.c` | `libretro` video, audio and timer drivers, modelled on the SDL2 ones |
| `src/Makefile.libretro` | the configure Makefile's object list minus `main_unix.o` |

The emulator changes are all `#ifdef LIBRETRO`:

- `start.c` registers the two drivers, skips installing signal handlers, and
  returns from `zesarux_main()` instead of entering the main loop.
- `timer_pause_waiting_end_frame()` returns instead of sleeping 1 ms.
- `FRAMES_VECES_BUFFER_AUDIO` is 1, so audio is sent every frame instead of
  every five.
- `debug.c` does not open the menu on errors.

Two things are worth knowing:

- **Pacing.** The libretro timer never fires on its own. `retro_run` calls
  `timer_trigger_interrupt()` and steps `cpu_core_loop()` until
  `esperando_tiempo_final_t_estados` is set again. That is one 50 Hz frame, and
  no thread is involved, so unload is safe. Networking is compiled out, since
  otherwise the stats threads start at boot.
- **Audio** is ZEsarUX's own: signed 8 bit stereo at 15600 Hz, 312 samples per
  frame, widened to 16 bit. At 60 Hz the per-frame padding in `envio_audio()`
  gives the wrong sample count.

## Content

`.nex`, `.snx`, snapshots and tapes are smart loaded. An `.mmc`/`.img` is
used as the SD image instead and boots NextZXOS from it (not tested yet).
Writes to the image are never persisted. The frame is 704×608: ZEsarUX draws the Next at twice its 352×304 resolution, so hi-res
Layer 2 modes keep every pixel.
