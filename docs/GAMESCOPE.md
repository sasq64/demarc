# gamescope as a libretro core

Every backend in demarc is a picture source — step it, take its frame, put the frame on a
quad — except one. `src/wine_emu.rs` runs Windows demos by launching a fullscreen
[gamescope] *on top of* demarc, and says plainly what that costs: "shaders, the grid and
screenshots don't apply to it". It also gives up audio, input routing, and any reliable
knowledge of when the demo ended.

`external/gamescope/` now carries a second way of doing it. A new backend inside gamescope
composites the session into a shared buffer instead of onto a display, and a small
`gamescope_libretro.so` beside it hands those frames to demarc through the ordinary
libretro path. The demo becomes a view like any other — shaders, grid and screenshots all
apply — and the same machinery runs an HTML/JS release in an undecorated Chrome.

This file is the working reference: what exists, how to build and run it, what was learned
along the way, and what is still open.

---

## Quick start

```sh
git -C external/gamescope submodule update --init --recursive   # once
just gamescope-core                        # builds the compositor and the core
just gs demos/some-demo.exe                # a Windows demo, captured into demarc
just gs-web demos/thing.html               # an HTML/JS release through Chrome
```

A Windows demo has to opt in with `wine_capture=true`, because `WineEmu` is still the
default (see Status). A page does not: `src/newsys/web.rs` claims `.html`/`.htm` outright,
since nothing else here could ever run one.

Build prerequisites beyond demarc's own: `meson`, `vulkan-headers`, `glslang`, and the
wlroots build dependencies (`wayland-protocols`, `libseat`, the `xcb-*` set, `gbm`,
`libinput`, `libudev`, `pixman`, `libdecor`, `luajit`, `hwdata`). wlroots, libliftoff and
libdisplay-info come from the submodules and are built statically.

`just gs` passes `--no-silence`. That matters: demarc redirects fd 1 and 2 to `/dev/null`
to muzzle the cores (`silence_stdout` in `src/main.rs`), and the compositor is a *child*
of the core, so gamescope's and wine's diagnostics go the same way. Without the flag, a
session that fails to start fails silently.

To drive it by hand, past the `WindowsSystem` routing:

```sh
DEMARC_CORE_DIR=$PWD/external/gamescope/build-lr/src \
  cargo run --profile release-fast -- --no-silence \
    -x wine_capture=true -x gamescope_command=glxgears some.exe
```

---

## How it fits together

```
demarc                                    │  gamescope (child process)
  RetroCoreThreaded                       │
    gamescope_libretro.so                 │    steamcompmgr thread
      retro_load_game  ── fork/exec ──────┼──▶   paint_all()
      socketpair(SEQPACKET)  ◀── HELLO ───┼──      └─ CLibretroConnector::Present()
        + 3 dmabuf fds (SCM_RIGHTS)       │             vulkan_screenshot() ─▶ ring[slot]
      retro_run        ◀── FRAME{slot} ───┼──          vulkan_wait()
        video_refresh(map[slot], pitch)   │
                       ─── RELEASE ──────▶┼──      ring slot free again
      keyboard/mouse   ─── INPUT ────────▶┼──▶   backend thread: wlserver_key() &c.
```

Three pieces:

| | where | what |
|---|---|---|
| backend | `external/gamescope/src/Backends/LibretroBackend.cpp` | `CLibretroBackend` / `CLibretroConnector`. The headless backend with an exit. |
| protocol | `external/gamescope/src/libretro/gamescope_libretro_ipc.h` | shared by both ends; the only thing they have in common |
| core | `external/gamescope/src/libretro/core.cpp` | the `.so` demarc loads. Links libc and libstdc++ and nothing else. |

On demarc's side: a branch in `WindowsSystem::create` (`src/newsys/windows.rs`) for the
wine path, and `WebSystem` (`src/newsys/web.rs`) for pages. `WebSystem` claims only the
page itself — a release ships its `.js`, textures and shaders beside it, Chrome fetches
those over `file://`, and they must stay available to the image and music systems.

### Why a separate process

gamescope is a hard singleton and hostile to being embedded. `g_device`, `g_output`,
`s_pBackend` and `wlserver` are file-scope globals; `main()` calls `exit()` on a bad
option, can `execv("/proc/self/exe")` to re-exec itself, `setenv`s `DISPLAY` and blanks
`WAYLAND_DISPLAY`, installs handlers for SIGINT/TERM/USR1/USR2, and on shutdown runs
`KillAllChildren(getpid(), SIGTERM)` — which inside demarc would kill demarc's children.
All of that is patchable, but only by carrying a large diff against upstream forever.

Running it as a child costs nothing in bandwidth, because the frames are shared rather
than copied (below), and buys crash isolation and more than one session at a time.

### Why it costs no extra copy

A `CVulkanTexture` created with `bMappable` is allocated `HOST_VISIBLE | HOST_COHERENT |
HOST_CACHED` and linear-tiled (`src/rendervulkan.cpp`), and with `bExportable` it also
carries a dmabuf. The backend allocates a ring of three such textures and sends their fds
over the socket once, at the hello; the core `mmap`s each one once. After that a frame is
a slot number.

So the chain is: the compositor writes the composite into host-visible memory (that is the
composite, not a copy), demarc's `convert_xrgb8888` reads that mapping straight into its
`frame: Vec<u32>`, `RetroCoreThreaded` moves it down its channel, and wgpu uploads it.
Exactly what an in-process design would do. `src/pipewire.cpp` has one `memcpy` at this
point only because pipewire allocates its shm separately from the texture.

`DRM_FORMAT_XRGB8888` *is* `RETRO_PIXEL_FORMAT_XRGB8888` on a little-endian machine, so no
pixel conversion happens anywhere.

### Pacing

None of it is demarc's to schedule. `CBaseBackendConnector::FrameSync()` sleeps to the next
vblank off `g_nOutputRefresh`, which the backend sets from `-r`, so the compositor runs at
the rate `gamescope_refresh` asks for and posts a frame when it has one. `retro_run` takes
the newest frame if one arrived and otherwise passes `NULL` to `video_refresh` — a dupe,
which demarc accepts because it answers `GET_CAN_DUPE`, and which costs it no upload.

This is why `--speed-test` numbers are not throughput. It runs demarc unthrottled, so most
of the `retro_run` calls it counts are dupes: 877 frames in 2s against a session pacing at
60 Hz means the dupe path is cheap, not that anything rendered 438 times a second.

---

## Core options

Legacy v0 `SET_VARIABLES` strings, because demarc answers `GET_CORE_OPTIONS_VERSION` with
0 and rejects a v2 array outright. A value demarc already holds beats the announced
default, so `-x <key>=<value>` sets any of them to something not in the list — which is how
`gamescope_command` takes a whole command line.

| key | default | |
|---|---|---|
| `gamescope_resolution` | `800x600` | session size; both the output captured and what the client is told it has |
| `gamescope_refresh` | `60` | Hz. `50` for demos that want it |
| `gamescope_command` | — | `wine`, `chrome`, or a literal command to run instead |
| `gamescope_wineprefix` | — | `WINEPREFIX` for a wine client |
| `gamescope_expose_wayland` | `false` | give the client gamescope's Wayland socket instead of only Xwayland |
| `gamescope_wine_desktop` | `false` | reserved; not yet wired to `explorer /desktop=` |

`WindowsSystem` restates its own vocabulary into these in `capture_meta`
(`src/newsys/windows.rs`), so an entry keeps saying `wine_res` and an `overrides.toml`
written for the on-top backend means the same thing here. `wine_res=pick` is not a size and
is deliberately not passed through.

---

## What was learned

- **`Present()` does not receive an image.** It receives a `FrameInfo_t` — the layer list —
  and the backend either hardware-planes it or composites it. `CHeadlessConnector::Present`
  is `{ return 0; }`, i.e. it throws every frame away, which is why `--backend headless`
  produces nothing today. Ours composites; because `paint_all` already built the
  `FrameInfo_t` for us, nothing needs repainting first, which is the one way this is
  simpler than `paint_pipewire()`.
- **A core cannot find anything by looking beside itself.** demarc copies every core into a
  private temp directory before `dlopen` so two instances get separate globals, and
  `GET_LIBRETRO_PATH` returns that copy too. The compositor's build and install paths are
  baked into the core at compile time instead (`GAMESCOPE_BUILD_BIN` /
  `GAMESCOPE_INSTALL_BIN`); `GAMESCOPE_LIBRETRO_BIN` overrides both.
- **Chrome needs X11, not Wayland.** gamescope sets `WAYLAND_DISPLAY` to the empty string,
  and Chrome's Ozone reads "set" as "Wayland is available", tries to connect to `""` and
  exits. Giving it a real Wayland socket fixes that and introduces two worse problems: it
  refuses to use Vulkan on Wayland, and it draws its own decorations and ignores being told
  to go fullscreen. Under `--ozone-platform=x11` gamescope's window manager — which exists
  to make one window fill one screen — does the job.
- **`--force-device-scale-factor=1`.** Otherwise Chrome inherits the HiDPI scale of the
  desktop that launched demarc, and a page written for 800x600 arrives cropped.
- **`-W/-H` and `-w/-h` and `-f`.** `-W/-H` size the captured output, `-w/-h` size what the
  client is told it has, and `-f` makes it fullscreen. All three are passed, with the two
  sizes equal: there is no display to letterbox into, so scaling would only cost sharpness.
- **`exit()` in a core is a bug** (the same lesson as `docs/PCEM.md`). A core lives in the
  frontend's address space. The only `_exit` here is in the forked child after `execvp`
  fails, which is where it belongs. `nm -u ... | grep -w exit` should stay empty.
- **Teardown has an order, and getting it wrong leaks quietly.** gamescope is a `setsid()`
  session leader, so its tree shares one process group and dies with one signal — but the
  leader dying is not the group dying. Reaping gamescope and stopping there left
  `gamescopereaper` running, so the group must be `SIGKILL`ed unconditionally once the
  leader is gone. And `wineserver -k` has to happen *before* that, while the server it
  talks to is still alive: kill the group first and `winedevice.exe`, which `setsid()`s out
  of the group and survives everything, is orphaned beyond the reach of any later
  `wineserver -k` — verified by trying it by hand on a leftover and watching it ignore me.
  This is the same class of leak `wine_emu.rs` documents ("thirty-seven of them left by
  earlier sessions"). A session now tears down with nothing left behind.
- **Input wants no new plumbing.** `wlserver_key(evdev, down, time)` and friends take a
  `wlserver_lock()` and can be called from any thread, which is what `SDLBackend` already
  does from its own. The backend runs one reader thread rather than adding a waitable to
  `steamcompmgr`'s.

### Local changes to upstream gamescope

Kept as small as possible, so the tree stays diffable:

- `src/Backends/LibretroBackend.cpp`, `src/libretro/` — new.
- `src/backends.h`, `src/main.cpp` — the `Libretro` enum value, `--backend libretro`, the
  `--libretro-fd` option.
- `src/main.hpp` — declares `ShutdownGamescope()`, which was defined in `main.cpp` and
  declared nowhere, so nothing outside it could ask for a clean shutdown.
- `src/meson.build`, `meson_options.txt` — a `libretro_backend` feature.
- `src/wlserver.cpp` — one `#include <float.h>`. Upstream uses `DBL_MAX` without it and no
  longer compiles under GCC 16. Not related to any of the above.

---

## Status

Working, and verified by eye on captured frames:

- **glxgears** (X11/GL) — 229 real frames of 240, animating, correct channel order.
- **Chromium** (X11, HTML/JS canvas) — 213 of 260, fullscreen, undecorated, 1:1, and
  reached by `demarc thing.html` with no flags.
- **wine** (`notepad.exe`) — renders fullscreen in the session.
- **Keyboard injection** — `retro_keyboard_callback` → socket → `wlserver_key` → Xwayland →
  the client. Typing "hello demarc" at a page that echoes keys shows "hello demarc".
- **Through demarc** — the picture reaches a view, with the CRT shader applied to it.
- **Teardown** — after a wine session unloads, no `gamescope`, `Xwayland`,
  `gamescopereaper`, `wineserver` or `winedevice.exe` is left running.

Open:

1. **No audio.** gamescope has none — an exhaustive grep of `src/` finds only keycode
   names. The core reports silence and pushes silent samples so the frontend's audio clock
   still advances; a wine demo's sound goes to the user's speakers as it does under
   `wine_emu.rs` today. The intended fix is a private PipeWire null sink with the child's
   `PULSE_SINK` pointed at it, captured into `retro_audio_sample_batch`.
2. **No `demarc-autodlg.exe` yet.** The wine path runs `wine <exe>` directly, so a demo
   that opens a setup dialog will sit on it, and the core cannot tell when the demo itself
   ended. `wine_emu.rs` solves both with the dialog driver, and the same command should be
   built here. Until then `WineEmu` remains the default and `wine_capture=true` is opt-in.
3. **`gamescope_wine_desktop` is announced but not wired** to `explorer /desktop=`.
   Relatedly, `wineserver -k` on teardown is wholesale, exactly as `wine_emu.rs`'s
   `close_prefix` is: two wine sessions sharing one prefix cannot be closed independently.
4. **`retro_reset` does nothing.** The honest equivalent is relaunching the client.
5. **A URL is not a page yet.** `WebSystem` matches on extension, and a URL demarc
   downloads lands in the content-addressed cache under a name that has none. Chrome
   itself is happy with either (`BuildClient` passes an `http` path through unchanged);
   it is the routing that needs teaching.
6. **Not tested in a grid.** Each core instance forks its own compositor, so several should
   work; nobody has run two at once.
7. **No distribution story.** Like PCem, this core is not on the libretro buildbot. The
   answer is an `ALT_SOURCES` entry in `src/libloader.rs` and a release publishing
   `gamescope_libretro-linux-x86_64.zip` — but the core also needs its compositor, which no
   other core has to ship.

[gamescope]: https://github.com/ValveSoftware/gamescope
