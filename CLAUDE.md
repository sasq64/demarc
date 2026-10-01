# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`demarc` is a command-line emulator frontend for watching demoscene productions: it takes files,
or a demo database, figures out which machine each one belongs to, downloads/loads the right
libretro core, and plays them full screen through a CRT/LCD shader — optionally several at once in
a grid. One binary, Bevy 0.19 for the app/render loop, edition 2024, laid out as a Cargo
workspace — see *Crates* below.

## Important Notes

- Avoid long comments, and comments that describe current behaviour, even though there are many comments like that in the code.
- When given a coding task, prefer simple small changes. Fix problems if needed, but don't embellish (don't add information to the UI unless told).

## Commands

Standard cargo commands

Good profile for testing:
```sh
cargo build --profile release-fast    
```

## Testing changes

Run headless with

`<files> --headless --remote-control <lua_script>`

See `scripts/remote_example.lua`

*NOTE* Avoid running normally since it opens windows and plays audio.


## Architecture

### Crates

The repo root is both the workspace root and the `demarc` package, so `src/` is demarc's own.
The three crates under `crates/` hold the parts that do **not** need Bevy, which is what lets the
libretro layer be built for Android (`docs/ANDROID.md`):

| crate | what | may depend on |
|---|---|---|
| `retro-core` | `backend.rs` (`trait Backend`), `libretro.rs` (bindgen), `pixels.rs`, `retro_emu/`, `find_core`, and the `c64` player behind the optional `player` feature | anyhow, libloading, tempfile, tracing, dirs — nothing else, deliberately |
| `newsys` | everything between a file on disk and a `Box<dyn Backend>`: `newsys.rs` + the systems, cache/fetch/libloader, the image and music backends, the wine layer, utils/workfile/m3u/system_dir | `retro-core` |
| `retroarc` | `.slangp` filter chains on plain wgpu. The only crate that links librashader | wgpu, librashader |
| `demarc` (root) | the Bevy app: frontend, emulator, UI, post-process, jobs, config | all three |

Dependencies point one way only. `demarc/src/main.rs` re-exports the moved modules at its crate
root, so `crate::backend`, `crate::newsys`, `crate::utils` etc. still resolve inside demarc.

Two rules keep the split honest: **nothing under `crates/` may mention Bevy**, and **`retro-core`
may not gain a dependency that does not cross-compile to `aarch64-linux-android`** (this is what
keeps `utils.rs` out of it — `unarc-rs` does not build for bionic).

### Layers

```
main.rs            CLI (clap, src/config.rs) → Bevy App + plugins;
  frontend.rs      FrontendPlugin: spawns one Emulator entity per view, grid layout, run_retro main system
    emulator.rs    Emulator component: pacing, input routing, audio sink, frame → Handle<Image> upload
      backend.rs   `trait Backend` (retro-core) — the only thing the frontend knows about a "core"
  post_process.rs  compositing of every view into one camera; SlangChains wraps `retroarc::Chains`
    post_process/  geometry.rs (where a view lands), composite.rs (the pass)
  egui_ui.rs       HUD, info overlay, fuzzy-search selector (fuzzy_list.rs)
  commands.rs      RightAlt/RightCtrl hotkeys → Cmd enum → app actions
```

`Backend` implementations, all interchangeable from the frontend's point of view:

| impl | file | notes |
|---|---|---|
| libretro core | `crates/retro-core/src/retro_emu.rs` + `retro_emu/threaded.rs` | `RetroCoreDirect` is the raw FFI/environment-callback side; `RetroCoreThreaded` runs it on a worker thread and ships frames back over a channel. This is what almost everything uses. |
| still image | `crates/newsys/src/image_emu.rs` | IFF/ILBM (`ilbm.rs`), DEGAS (`degas.rs`), ZX SCR (`zx_scr.rs`), palette TIFF (`tiff_pal.rs`, which the `image` crate refuses), plus `image` crate formats; optional palette colour-cycling |
| music | `crates/newsys/src/music_emu.rs` | `musix` chiptune/tracker player, renders audio inline (no worker thread) and draws a Luau visualizer (`music_vis.rs`) |
| Flash | `flash_emu.rs` | behind the `flash` feature; Ruffle with its own wgpu device |
| gamescope session | `external/gamescope/src/libretro/` (C++) | Linux only. A patched gamescope composites a headless Wayland/Xwayland session into a shared dmabuf and a thin `gamescope_libretro.so` hands the frames back, so wine — or an HTML/JS release in an undecorated Chrome — is a picture source like any other. What to run inside it is `src/wine.rs`'s job for a Windows release (the wine command, the prefix, its teardown), restated as core options by `src/newsys/windows.rs`. See `docs/GAMESCOPE.md`. |
| Windows release, on Windows | `win_runner.rs` | Windows only. Nothing is emulated and nothing is captured: the demo is started on the desktop and demarc only holds its process, with `demarc-autodlg.exe` answering the setup dialog as it does under wine. |

### A second binary: `c64`

`crates/retro-core/src/player.rs` is a stripped-down player — one 720x576 window, the
VICE core, `system/shaders/lottes.wgsl` — on winit + wgpu with no Bevy, no librashader
and no command line, as the starting point for an Android port. `src/bin/c64.rs` beside
it is only its desktop `main`. It is behind the optional `player` feature, so demarc —
which wants the same crate for its libretro layer — builds none of winit or wgpu twice.
The shader is the same file the Bevy path uses, with its one `#import` line swapped for
a fullscreen vertex shader.

Build it for a phone with:

```sh
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs \
    build -p retro-core --features player --lib --release
```

which produces a 16 KB-page-aligned `libretro_core.so` needing only
libdl/libandroid/liblog/libc. `scripts/run-android.sh` packages that plus the VICE core
into an APK (the Gradle project is `android/`), installs and launches it; it boots to the
BASIC banner. See `docs/ANDROID.md` for what is still missing (touch input, audio).

### System detection — `crates/newsys/src/newsys.rs` + `src/newsys/*`

`trait System` is the per-machine knowledge: which extensions/headers it claims (`can_load`),
what to do to a release before it can boot (`load`, working on a mutable `WorkFile`), which libretro
core to use (`core_name`), default core options (`default_meta`), and how to build the backend
(`create`, whose default just spins up `RetroCoreThreaded`).

`NewSys::load_file` is the whole pipeline: unpack archives (twice, releases are often double-packed) →
read m3u tags → apply `overrides.toml` → apply `-x` CLI meta → try each `System` in the order listed
in `NewSys::get_systems` until one claims the file → fill in that system's `default_meta` for keys
nothing else set → `create` the backend. **Order matters** — specific systems come first,
`MusicSystem` and `ImageSystem` last because `musix` and `image` claim a lot of files.

It is split in two so the frontend can run the halves as separate jobs: `newsys::unpack_release`
(unpack + m3u tags) needs nothing but the file, so `Emulator::load_async` runs it on the I/O pool
along with the download, and `NewSys::load_prepared` (everything from the override onwards) follows
on a second I/O-pool job — `Emulator::start_create`, which also drops the outgoing core there.
Neither half touches the main thread: unpacking cost a dropped frame, and `retro_load_game` plus a
core teardown cost tens to hundreds of milliseconds. So a `System` must be usable off the main
thread, and `NewSys` holds its run-wide meta behind a lock.
`load_file` is still the two called in order, and is what the tests use.

To add a machine: new file under `crates/newsys/src/newsys/`, implement `System`, register it in `get_systems()`.

### Meta

Everything system-specific travels as string key/value "meta" on the `WorkFile`, and keys that a
libretro core announced via `SET_VARIABLES` become that core's options (a value we already hold beats
the core's announced default). Precedence, weakest first: core default → `System::default_meta` →
db line / m3u tags → `overrides.toml` → `-x key=value` on the command line.

`src/overrides.rs` (demarc) parses the toml; the `Override`/`Patch` types it builds live in
`newsys::release_override`, which is what the systems read. It holds per-release fixups keyed on demozoo id (which file to fetch, which file to
boot, files to patch in, AmigaDOS assigns, core options), read from `system/overrides.toml`.

### Files, downloads, cores

- `files.rs` — collects `EmuFile`s from paths, directories and tab-separated demo databases
  (`bitworld.txt`, `csdb.txt`, `demozoo.txt`, optionally gz/bz2). The db text is deliberately leaked
  so entries can hold `&'static str` slices into it.
- `emu_file.rs` — a `FileSource` is either a local path or a list of URLs resolved on first load.
- `jobs.rs` — blocking work (download, unpack) on Bevy's `IoTaskPool`, polled from ordinary systems.
  There is intentionally no async runtime; read the module docs before adding one.
- `fetch.rs` / `cache.rs` — HTTP/FTP fetching into a content-addressed, size-bounded cache split by
  entry size so tunes and CD images don't evict each other.
- `newsys/libloader.rs` — downloads cores from the libretro nightly buildbot on demand, or from `ALT_SOURCES`
  for the ones the buildbot doesn't ship (amiberry, fake08). `DEMARC_CORE_DIR` overrides both, which
  is how you test a locally built core.

### Generated / vendored code

- `crates/retro-core/src/libretro.rs` (223k, `#[allow(warnings)]`) is **bindgen output — never
  hand-edit**. Regenerate
  with `scripts/gen-libretro-bindings.sh`; it's committed so no build machine needs libclang.
- The `libretro/pt2-clone` checkout is a fork that carries the ProTracker 2 clone as a
  libretro core: `libretro/` there holds our `retro_*` entry points and an SDL2 replacement,
  built (`just pt2-core`) against the unmodified tracker sources beside it. See `docs/PT2.md`.
  `libretro/ft2-clone` does the same for the Fasttracker II clone (`just ft2-core`, `docs/FT2.md`).
- `libretro/`, `slang-shaders/` and `shaders/` are gitignored working checkouts, not part of
  the repo. `shaders/` is the RetroArch-shaped layout the Mega Bezel preset packs need — see
  `docs/SHADERS.md`, which also explains why `librashader` is pinned to a fork.
- `external/ADFlib` and `external/dms` are vendored C, built by `crates/newsys/build.rs` and
  reached through the shims in `crates/newsys/src/c_shims/adf_unpack_shim.c` /
  `dms_unpack_shim.c` (Rust side: `newsys/adf.rs` and `newsys/dms.rs`). Both serve `--unadf`: ADFlib walks a disk image's file system, xDMS turns a
  `.dms` archive back into that image first. The xDMS sources are amiberry's copy, and only
  `pfile.c` was edited — keep the rest diffable against
  `external/amiberry/src/archivers/dms`.
- There are two build scripts. `crates/retro-core/build.rs` compiles `retro_log_shim.c` and
  nothing else — keep it that way, it has to run under the NDK. `crates/newsys/build.rs` compiles
  the rest of the vendored C/C++ (`external/cbmconvert`, ADFlib, xDMS, an unrar shim needed only
  when cross-compiling to Windows) and packs `system/` into an embedded `system.zip`
  (checksum-cached). Its paths are `../../`-relative to reach the repo root, and `rel_name` strips
  that prefix back off so the archive's entry names and `SKIP_DIRS` stay repo-relative.
  `system_dir()` prefers a local `system/` directory in debug builds and otherwise unpacks the
  embedded zip into the user cache — so editing `system/` works directly during development.
  Most cores get `system_dir()` itself, but the Amiga ones get `system/amiga/` (`amiga_system_dir()`
  in `newsys/amiga.rs`): amiberry recursively scans and content-probes everything under the
  directory it is handed, and used to abort on another core's data. Put per-core assets in their
  own subdirectory, and add anything a core *writes* there to `SKIP_DIRS`/`MARKER_FILES` in
  `crates/newsys/build.rs` so a debug run does not pack it back into `system.zip`.

## Conventions

- Unit tests live beside the code as a `mod tests` declared out of line: a `tests/` directory next
  to the source file, holding one `<module>_tests.rs` per module, pulled in with
  `#[cfg(test)] #[path = "tests/<module>_tests.rs"] mod tests;` at the bottom of the file
- A test that reaches repo-root content (`system/`, `testdata/`) uses `env!("DEMARC_ROOT")`, not
  `env!("CARGO_MANIFEST_DIR")` — the latter points at the crate. `DEMARC_ROOT` is set for the whole
  workspace in `.cargo/config.toml`.

## Docs worth reading before touching those areas

`docs/AMIBERRY.md` (Amiga core options/WHDLoad), `docs/86BOX.md`, `PICO8.md`,
`GAMESCOPE.md`, `PT2.md` and `FT2.md` (the non-buildbot cores), `docs/flags.md` (core option reference tables), `docs/SHADERS.md`
(`--slangp` presets, the librashader fork, Mega Bezel packs — the code is `crates/retroarc`),
`docs/CRINKLER.md` (why 4k
intros run but draw nothing under wine), `docs/NOTES.md` (design
scratchpad for the loading pipeline), `docs/ANDROID.md` (the plan for putting the `c64`
binary on a phone, and why that needs the package split up), `docs/TODO.md` and
`AI_TASKS.md` (open work), `CHANGELOG.md`.

## Releases

`dist` builds Linux/Windows/macOS artifacts when a `v<version>` tag is pushed. The tag must match
`version` in `Cargo.toml` exactly, prerelease suffix included. See the RELEASE section of `README.md`.
