# pt2-clone as a libretro core

[pt2-clone](https://github.com/8bitbubsy/pt2-clone) is Olav Sørensen's
ProTracker 2.3D clone: an SDL2 program, BSD licensed, that is the tracker
screen as well as the replayer. The `libretro/` directory in our fork turns it
into `pt2clone_libretro.so`, so a module can play through the real ProTracker
mixer with the real ProTracker screen — pattern, scopes, VU meters and all — instead
of through [`MusicEmu`](../src/music_emu.rs).

---

## Quick start

```sh
git clone -b libretro https://github.com/sasq64/pt2-clone libretro/pt2-clone
just pt2-core
just pt2 mod.something
```

`libretro/` is a directory of working checkouts and is not part of this repo.
The fork builds the core on its own — `cmake -S libretro -B build` inside the
checkout is all `just pt2-core` does, and `PT2_CLONE_DIR` overrides where the
tracker's sources are read from. SDL2's **headers** have to be installed, and
that is all the core takes from SDL: nothing links against the library.

In demarc, the `use_protracker` meta sends ProTracker modules to the core
rather than to `MusicEmu` — see `src/newsys/music.rs`:

```sh
demarc -x use_protracker=true mod.something
```

`scripts/libretro-smoke.py libretro/pt2-clone/libretro/build/pt2clone_libretro.so mod.x`
drives the core with no Bevy in the way, which is the quickest way to tell a
core problem from a frontend one.

---

## How it works

The tracker's sources are compiled **unmodified**. What changes is what SDL2
means:

| File | Contents |
|---|---|
| `pt2_libretro.c` | `retro_*` entry points, the thread `main()` runs on, libretro key and mouse input translated to SDL's |
| `sdl_shim.c` | every SDL2 call pt2-clone makes, backed by libretro instead of by a window and an audio device |
| `pt2_compat.h` | force included into every translation unit: renames `main()`, and takes away the two things a program may do that a shared library in someone else's process may not |
| `link.T`, `link.exp` | the export list, `retro_*` and nothing else, for ld and for ld64 |

`main()` keeps running its own loop on a thread of its own. `SDL_RenderPresent`
is where that thread parks between frames, so `retro_run` asking for a frame is
what lets the tracker draw the next one, and the frontend stays in charge of
the pace without anything in pt2-clone knowing about it. The pixels are copied
out while that thread is parked, so there is nothing to race with.

Audio is mixed inside `retro_run`, one frame's worth per call, just before the
frame that goes with it is taken. That is not an optimisation, it is the sync:
the replayer runs inside pt2-clone's audio callback, so mixing is what moves the
song on, and anything buffered between the callback and the frontend would land
the sound later than the picture drawing it. Calling the callback on the
frontend's thread is safe because the tracker is parked in `SDL_RenderPresent`
while it runs, and no `lockAudio()` region in pt2-clone draws a frame.

The clock is the other half of it. pt2-clone holds its scopes and meters back
in a queue until the audio behind them is due, and it stamps that queue from
`SDL_GetPerformanceCounter()` — real time, which is right for a program that
owns the audio device and wrong for a core, where the song advances when the
frontend runs us and the samples are played whenever the frontend gets to them.
So the counter reports **how much audio has been mixed** instead, to everything
but the threads pt2-clone starts through `SDL_CreateThread` (they use it to pace
themselves, and that really is real time). The queue then releases the picture
for the samples that just went out, however fast or slow the frontend is going.
For the same reason the device reports a buffer size of zero: the latency it
stands for is the frontend's, and the frontend lines the picture up with it
itself.

`pt2_compat.h` is where the rest of "this is a library now" lives:

- `main` becomes `pt2_clone_main`, so the core can call it.
- `chdir` does nothing. The working directory belongs to the frontend, which
  resolves relative paths of its own while the core runs, and the tracker walks
  it looking for `protracker.ini` and for the disk op. directory. Without a
  config file the tracker's own defaults apply, which is what we want anyway.
- The crash handler is not installed. It writes a backup module into whatever
  directory it lands in and hands the signal back, neither of which is a core's
  business.
- On Windows the tracker is compiled with `_DEBUG` (and `NDEBUG` beside it, so
  its `ASSERT()` stays a no-op). That is pt2-clone's own switch for the
  process-wide low-level keyboard hook, the unhandled exception filter and the
  `chdir` to the .exe's directory; the modal error box and the DPI call are
  macros in `pt2_compat.h`, and single instancing stops at a
  `SDL_GetWindowWMInfo` that has no window to report.

Settings a core has no use for — fullscreen, vsync, a hardware mouse pointer,
the audio rate and buffer size — are set in `SDL_CreateWindow`, which is the one
call the tracker makes after reading its config and before setting anything up
from it.

---

## Releases

`.github/workflows/libretro.yml` in the fork builds the core for Linux x86_64,
macOS arm64 and Windows x64 on every push to its `libretro` branch and publishes
the three zips to a rolling `latest` release, the same shape amiberry and
gamescope use. That release is where `libloader::ALT_SOURCES` fetches
`pt2clone` from, so nothing has to be tagged for a new build to reach demarc.

---

## Notes

- The screen is 320x255 at 60Hz, which is what the tracker draws and what the
  core reports. Music timing comes from the replayer, not from the frame rate.
- A/V sync is testable without listening: give a module a note every few rows
  and a `C00` after it, then check that the audio `retro_run` hands over and the
  scope in the frame it hands over change in the same call. Because nothing is
  keyed to the wall clock, that holds at `scripts/libretro-smoke.py` speed —
  thousands of runs a second — as well as at 60Hz.
- Keyboard and mouse both work, so the tracker is usable, not just watchable:
  the mouse is libretro's relative mouse integrated into a position on the
  320x255 screen, and the left and right Amiga keys are the super/meta keys.
- Nothing is written anywhere. There is no config file to load and none to
  save.
