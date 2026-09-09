# gamescope as a libretro core

Every backend in demarc is a picture source — step it, take its frame, put the frame on a
quad — and Windows demos are no exception, though it takes a compositor to make them one.
demarc used to run them by launching a fullscreen [gamescope] *on top of* itself, which
cost shaders, the grid, screenshots, audio, input routing and any reliable knowledge of
when the demo ended.

`external/gamescope/` is how they are run now. A new backend inside gamescope composites
the session into a shared buffer instead of onto a display, and a small
`gamescope_libretro.so` beside it hands those frames to demarc through the ordinary
libretro path. The demo is a view like any other — shaders, grid and screenshots all
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

`WindowsSystem` routes a `.exe` here by itself, and hands the core the whole command to
run inside the session — the dialog driver, the resolution, a virtual desktop if the entry
asked for one. So does `src/newsys/web.rs` for a page, which claims `.html`/`.htm`
outright since nothing else here could ever run one.

Run-time prerequisites for the Windows half are `wine`, `bwrap` and the provisioned
`~/.wine-demarc` prefix (`just wine-prefix`). `demarc --check-wine` says which of the
three are there and exits — 0 when all of them are. Without all three `WindowsSystem` is
left out of the system list at startup (`check_wine` in `src/wine.rs`), so a `.exe` falls
through to whatever else can claim it rather than opening an empty session.

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
    -x gamescope_command=glxgears some.exe
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
wine path, with `capture_meta` beside it turning the entry's settings into core options and
its wine command into `gamescope_command`, and `WebSystem` (`src/newsys/web.rs`) for pages. `WebSystem` claims only the
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
| `gamescope_command` | — | `wine`, `chrome`, or a literal command to run instead. Split on ASCII US (`\x1f`) when it holds one — which is how demarc sends a whole argv whose paths have spaces in them — and on whitespace otherwise, which is what a hand-typed `-x gamescope_command="vkcube --gpu 0"` wants |
| `gamescope_wineprefix` | — | `WINEPREFIX` for a wine client. Sandboxed, this is the session's own throwaway overlay rather than `~/.wine-demarc` itself |
| `gamescope_close_prefix` | `true` | run `wineserver -k` on that prefix at unload. demarc sends `false` for a sandboxed session, which has nothing out here to close |
| `gamescope_wine_dll_overrides` | — | `WINEDLLOVERRIDES` for a wine client, wine's own syntax (`d3dx9_37=n`). demarc fills it in from the DLLs a release ships beside its `.exe` |
| `gamescope_mesa_gl_version_override` | — | `MESA_GL_VERSION_OVERRIDE` for the client. demarc sets it to `4.6COMPAT` when an entry says `wine_gl_compat` |
| `gamescope_expose_wayland` | `false` | give the client gamescope's Wayland socket instead of only Xwayland |

`WindowsSystem` restates its own vocabulary into these in `capture_meta`
(`src/newsys/windows.rs`), so an entry keeps saying `wine_res`, `wine_desktop`,
`wine_dll_overrides` and `wine_gl_compat`, and an `overrides.toml` written before any of
this existed means the same thing here.

The command is the substantial half of that translation. Left to itself the core turns a
`.exe` into `wine <exe>`, which is a demo sitting on its setup dialog with nobody to answer
it; what it is given instead is the argv `crate::wine::wine_command` builds, dialog driver
and all — everything demarc knows about starting a Windows release lives in `src/wine.rs`,
and the core is handed the result. `wine_desktop` rides along inside it as
`explorer /desktop=`, which is why the core has no option of its own for it. `wine_res=pick`
is not a size, so the resolution passed is the stand-in for it (1920x1200, big enough to
hold whatever the person watching chooses).

### The prefix each session runs in

The command demarc sends is not `wine ...` but `bwrap ... -- wine ...`. `src/wine_sandbox.rs`
puts the real `~/.wine-demarc` underneath an overlay whose upper layer is an invisible
tmpfs, mounts it at a path this session alone uses, gives the session a private
`/tmp/.wine-<uid>` and a pid namespace of its own, and points `WINEPREFIX` at the result:

```sh
bwrap --dev-bind / / --proc /proc --unshare-pid --die-with-parent \
      --perms 0700 --tmpfs /tmp/.wine-1000 \
      --overlay-src ~/.wine-demarc \
      --tmp-overlay /run/user/1000/demarc-wine-1000/4711/0 \
      --setenv WINEPREFIX /run/user/1000/demarc-wine-1000/4711/0 \
      -- wine demarc-autodlg.exe --launch demo.exe --prefer 800x600 --check Fullscreen
```

That is what makes a grid of Windows demos work. wine names its server socket after the
prefix's device and inode under `/tmp/.wine-<uid>`, so two sessions in one prefix are two
clients of one wineserver and `wineserver -k` closes both; each session having its own
prefix *and* its own socket directory makes them two servers that know nothing of each
other. The pid namespace is the other half: wine's services `setsid` out of any process
group, which is why both backends carry code to hunt them down, and inside a namespace
there is nowhere to go — when the demo exits, the kernel takes `wineserver`,
`services.exe` and `winedevice.exe` with it. Writes land in the tmpfs and are gone with
the session, so a demo cannot damage what `just wine-prefix` installed either.

It is not a security boundary and does not try to be: `--dev-bind / /` hands the demo the
whole host filesystem, because it needs the GPU nodes, the audio socket, gamescope's X
socket and its own directory. Containment of writes and of processes is the point.

`wine_sandbox=false` turns it off, and so does a machine without `bwrap` or without
unprivileged user namespaces and overlayfs — demarc probes for that once per run with the
real argument list and `true` in place of the demo, and falls back to the shared prefix,
one demo at a time, as it worked before. The first run on a machine with no
`~/.wine-demarc` yet also falls back: there would be nothing to overlay, and a prefix
built inside a tmpfs is one thrown away again.

`wine_gl_compat` is the other translation worth knowing about. A GL demo of the 2010s asks
for a 3.x context and leaves the profile mask out, which per spec means *core* — and a core
context does not advertise `GL_ARB_multitexture` or the rest of the pre-3.0 extension
strings. Wine's `wglGetProcAddress` checks that the extension a name belongs to is on the
current context before it resolves it, so `glActiveTextureARB` comes back NULL where a
Windows ICD would have handed over a pointer to `glActiveTexture`; an intro that resolves
its entry points into a table without checking them then calls straight through the NULL.
Setting `MESA_GL_VERSION_OVERRIDE=4.6COMPAT` on the client asks Mesa for a compatibility
context, which puts the strings back and lets the aliases resolve. Approximate's *Gaia
Machina* (`zoo.31427`) is the worked example: without it, a page fault at address 0 during
FBO setup, every time.

---

## What was learned

- **`Present()` does not receive an image.** It receives a `FrameInfo_t` — the layer list —
  and the backend either hardware-planes it or composites it. `CHeadlessConnector::Present`
  is `{ return 0; }`, i.e. it throws every frame away, which is why `--backend headless`
  produces nothing today. Ours composites; because `paint_all` already built the
  `FrameInfo_t` for us, nothing needs repainting first, which is the one way this is
  simpler than `paint_pipewire()`.
- **A core cannot find anything beside the copy it was loaded from.** demarc copies every
  core into a private temp directory before `dlopen` so two instances get separate
  globals, and nothing is unpacked beside that copy. `GET_LIBRETRO_PATH` now answers with
  the core as it lives on disk rather than the copy — which is what the callback means,
  and what lets `FindGamescope()` pick up the compositor a downloaded release unpacked
  next to the library. Failing that the build and install paths baked in at compile time
  (`GAMESCOPE_BUILD_BIN` / `GAMESCOPE_INSTALL_BIN`) still answer, which is what a local
  build uses; `GAMESCOPE_LIBRETRO_BIN` overrides everything.
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
  This is the same class of leak `sweep_prefix` in `src/wine.rs` documents ("thirty-seven
  of them left by earlier sessions"). A session now tears down with nothing left behind.
- **A command is an argv, not a string.** `gamescope_command` is one core-option string,
  and the wine command demarc builds has two paths in it — the demo's and the driver's —
  both of which routinely contain spaces, brackets and apostrophes. Splitting it back up on
  whitespace would tear those in half, and quoting rules would mean writing a shell. ASCII
  US between the words instead: it exists for this, cannot occur in a path, and leaves the
  whitespace split in place for commands people type by hand.
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

This is what a Windows entry gets, and the only way demarc runs one; the on-top backend
that came before it is gone. `gamescope` is an `ALT_SOURCES` entry (below), so an ordinary
demarc downloads it like any other core.

Working, and verified by eye on captured frames:

- **glxgears** (X11/GL) — 229 real frames of 240, animating, correct channel order.
- **Chromium** (X11, HTML/JS canvas) — 213 of 260, fullscreen, undecorated, 1:1, and
  reached by `demarc thing.html` with no flags.
- **wine** (`notepad.exe`) — renders fullscreen in the session.
- **A Windows demo with a setup dialog** (fr-025) — the driver answers the dialog,
  launches the demo, undecorates its window and reports
  `!demarc started`; the demo plays inside a demarc view at the session size.
- **Keyboard injection** — `retro_keyboard_callback` → socket → `wlserver_key` → Xwayland →
  the client. Typing "hello demarc" at a page that echoes keys shows "hello demarc".
- **Through demarc** — the picture reaches a view, with the CRT shader applied to it.
- **Teardown** — after a wine session unloads, no `gamescope`, `Xwayland`,
  `gamescopereaper`, `wineserver` or `winedevice.exe` is left running.
- **Two Windows demos at once** — `--grid=2x1 heaven7.exe tracie.exe`
  brings up two compositors, two sandboxes, two wineservers and two demos rendering side
  by side in demarc's grid. See The prefix each session runs in.

Open:

1. **No audio.** gamescope has none — an exhaustive grep of `src/` finds only keycode
   names. The core reports silence and pushes silent samples so the frontend's audio clock
   still advances; a wine demo's sound goes straight to the user's speakers. The
   intended fix is a private PipeWire null sink with the child's
   `PULSE_SINK` pointed at it, captured into `retro_audio_sample_batch`.
2. **The end of a demo is noticed late.** `demarc-autodlg.exe` is now in the command, so
   the setup dialog gets answered and the driver writes `!demarc started` / `exited` as it
   always has — but the core inherits gamescope's stdout rather than reading it, so nobody
   here sees those lines. What ends a captured session instead is demarc's ordinary idle
   detection: the compositor keeps presenting the same empty frame once the demo is gone,
   and a frozen, silent view is one the frontend moves on from. Reading the driver's stream
   in the core would make it prompt, and would tell a demo that failed to start from one on
   a long loading screen.
3. **`retro_reset` does nothing.** The honest equivalent is relaunching the client.
4. **A URL is not a page yet.** `WebSystem` matches on extension, and a URL demarc
   downloads lands in the content-addressed cache under a name that has none. Chrome
   itself is happy with either (`BuildClient` passes an `http` path through unchanged);
   it is the routing that needs teaching.
5. **Chrome sessions still share one profile directory.** `ProfileDir()` is one path under
   the save directory, so two pages at once fight over it — the wine half of this is
   solved (see The prefix each session runs in), the Chrome half is not.
6. **The release has not been run on a machine that did not build it.** See
   Distribution — the bundle is built against Ubuntu 24.04's libraries and carries the
   ones a desktop cannot be assumed to have, but nobody has yet unpacked it on a
   different distribution and started a session from it.

---

## Distribution

`external/gamescope/.github/workflows/libretro.yml` builds the compositor and the core on
every push to the `demarc` branch and publishes them to a rolling `latest` release, the
same shape amiberry's workflow uses. `src/libloader.rs` fetches it from there:

```
https://github.com/sasq64/gamescope/releases/download/latest/gamescope_libretro-linux-x86_64.zip
```

The zip is unpacked into the core cache and holds three things:

```
gamescope_libretro.so   the core — the only file libloader looks for
gamescope               the compositor it forks, found through GET_LIBRETRO_PATH
lib/                    the libraries a current desktop cannot be assumed to have
```

`lib/` is what makes this core unlike every other one, and it is deliberately small.
wlroots 0.20 wants libraries newer than any LTS ships (libdrm 2.4.129, wayland 1.24,
wayland-protocols 1.47, xkbcommon 1.8, pixman 0.46), so the workflow builds those five
from pinned releases and ships them; alongside them go libinput, libseat, libdecor,
libdisplay-info and luajit, which plenty of desktops do not have installed at all. What is
*not* shipped is everything the host must provide anyway or must own: the C/C++ runtime,
the graphics stack the host's Vulkan driver is built against, X11, and the systemd/glib
layer. They are reached through a `DT_RPATH` of `$ORIGIN/lib` rather than an
`LD_LIBRARY_PATH`, because the compositor execs wine and Chrome and an `LD_LIBRARY_PATH`
would follow them into processes that must use the host's libraries.

Built on Ubuntu 24.04, so the release needs glibc 2.39 or newer — Ubuntu 24.04, Debian 13,
Fedora 40, SteamOS 3.7, Arch. From the host it also needs a Vulkan driver, `Xwayland`, and
whatever the session runs (`wine`, `google-chrome`).

A locally built core still wins over all of this: `DEMARC_CORE_DIR` pointing at
`external/gamescope/build-lr/src` is unchanged, and there the compositor is found beside
the library exactly as it is in a release.

[gamescope]: https://github.com/ValveSoftware/gamescope
