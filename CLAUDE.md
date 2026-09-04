# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`demarc` is a command-line emulator frontend for watching demoscene productions: it takes files,
URLs or a demo database, figures out which machine each one belongs to, downloads/loads the right
libretro core, and plays them full screen through a CRT/LCD shader — optionally several at once in
a grid. Single Rust binary, Bevy 0.19 for the app/render loop, edition 2024.

## Commands

```sh
cargo build --release                 # ship build (lto, stripped)
cargo build --profile release-fast    # what you want while iterating (opt-level 2, no LTO, incremental)
cargo run --profile release-fast -- --window demos/rebels.adf
cargo test                            # ~320 unit tests
cargo test <name>                     # single test / substring filter
cargo test -- --ignored               # network, GPU and locally-built-core tests
cargo clippy
```

Handy `just` recipes: `just run|ami|c64|gb|iff|royale` (launch a sample), `just test`, `just clippy`,
`just coverage`, `just profile` + `just trace-summary` (Bevy per-system spans → `trace.json`),
`just win` (cross-build the Windows exe with cargo-xwin), `just pcem-core` / `just pc <cfg>`,
`just release-check` / `release-local`, `just pal|ntsc|native` (flip the Hyprland monitor to 50/60Hz —
demos want 50Hz).

Ignored tests are ignored for a reason: they hit the network, need a GPU adapter, or need a locally
built PCem core plus BIOS ROMs. Don't un-ignore them to "fix" a red run.

`--speed-test` runs emulation unthrottled for a fixed window and prints a frame count; it's the
benchmark to quote when changing anything in the emulation or upload path.

## Architecture

### Layers

```
main.rs            CLI (clap, src/config.rs) → Bevy App + plugins; stdout muzzling; rlimit/malloc tuning
  frontend.rs      RetroPlugin: spawns one Emulator entity per view, grid layout, run_retro main system
    emulator.rs    Emulator component: pacing, input routing, audio sink, frame → Handle<Image> upload
      backend.rs   `trait Backend` — the only thing the frontend knows about a "core"
  post_process.rs  librashader/wgpu compositing of every view into one camera
  egui_ui.rs       HUD, info overlay, fuzzy-search selector (fuzzy_list.rs)
  commands.rs      RightAlt/RightCtrl hotkeys → Cmd enum → app actions
```

`Backend` implementations, all interchangeable from the frontend's point of view:

| impl | file | notes |
|---|---|---|
| libretro core | `retro_emu.rs` + `retro_emu/threaded.rs` | `RetroCoreDirect` is the raw FFI/environment-callback side; `RetroCoreThreaded` runs it on a worker thread and ships frames back over a channel. This is what almost everything uses. |
| still image | `image_emu.rs` | IFF/ILBM (`ilbm.rs`), DEGAS (`degas.rs`), ZX SCR (`zx_scr.rs`), plus `image` crate formats; optional palette colour-cycling |
| music | `music_emu.rs` | `musix` chiptune/tracker player, renders audio inline (no worker thread) and draws a Luau visualizer (`music_vis.rs`) |
| Flash | `flash_emu.rs` | behind the `flash` feature; Ruffle with its own wgpu device |
| Windows demos | `wine_emu.rs` | Linux only, and *not* a picture source: it launches wine inside gamescope on top of demarc, so shaders/grid/screenshots don't apply |

### System detection — `newsys.rs` + `src/newsys/*`

`trait System` is the per-machine knowledge: which extensions/headers it claims (`can_load`),
what to do to a release before it can boot (`load`, working on a mutable `WorkFile`), which libretro
core to use (`core_name`), default core options (`default_meta`), and how to build the backend
(`create`, whose default just spins up `RetroCoreThreaded`).

`NewSys::load_file` is the whole pipeline: unpack archives (twice, releases are often double-packed) →
read m3u tags → apply `overrides.toml` → apply `-x` CLI meta → try each `System` in the order listed
in `NewSys::get_systems` until one claims the file → fill in that system's `default_meta` for keys
nothing else set → `create` the backend. **Order matters** — specific systems come first,
`MusicSystem` and `ImageSystem` last because `musix` and `image` claim a lot of files.

It is split in two so the frontend can run the halves in different places: `newsys::unpack_release`
(unpack + m3u tags) needs nothing but the file, so `Emulator::load_async` runs it on the I/O pool
along with the download, and only `NewSys::load_prepared` (everything from the override onwards)
runs on the main thread — unpacking there was a dropped frame in the middle of a `--cross-fade`.
`load_file` is still the two called in order, and is what the tests use.

To add a machine: new file under `src/newsys/`, implement `System`, register it in `get_systems()`.

### Meta

Everything system-specific travels as string key/value "meta" on the `WorkFile`, and keys that a
libretro core announced via `SET_VARIABLES` become that core's options (a value we already hold beats
the core's announced default). Precedence, weakest first: core default → `System::default_meta` →
db line / m3u tags → `overrides.toml` → `-x key=value` on the command line.

`src/overrides.rs` holds per-release fixups keyed on demozoo id (which file to fetch, which file to
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
- `libloader.rs` — downloads cores from the libretro nightly buildbot on demand, or from `ALT_SOURCES`
  for the ones the buildbot doesn't ship (amiberry, fake08). `DEMARC_CORE_DIR` overrides both, which
  is how you test a locally built core.

### Generated / vendored code

- `src/libretro.rs` (223k, `#[allow(warnings)]`) is **bindgen output — never hand-edit**. Regenerate
  with `scripts/gen-libretro-bindings.sh`; it's committed so no build machine needs libclang.
- `libretro/` and `slang-shaders/` are gitignored working checkouts, not part of the repo.
- `external/ADFlib` and `external/dms` are vendored C, built by `build.rs` and reached through the
  shims in `src/adf_unpack_shim.c` / `src/dms_unpack_shim.c` (Rust side: `src/newsys/adf.rs` and
  `src/newsys/dms.rs`). Both serve `--unadf`: ADFlib walks a disk image's file system, xDMS turns a
  `.dms` archive back into that image first. The xDMS sources are amiberry's copy, and only
  `pfile.c` was edited — keep the rest diffable against
  `external/amiberry/src/archivers/dms`.
- `build.rs` compiles C/C++ (`retro_log_shim.c`, vendored `cbmconvert`, an unrar shim needed only when
  cross-compiling to Windows) and packs `system/` into an embedded `system.zip` (checksum-cached).
  `system_dir()` prefers a local `system/` directory in debug builds and otherwise unpacks the
  embedded zip into the user cache — so editing `system/` works directly during development.
  Most cores get `system_dir()` itself, but the Amiga ones get `system/amiga/` (`amiga_system_dir()`
  in `newsys/amiga.rs`): amiberry recursively scans and content-probes everything under the
  directory it is handed, and used to abort on another core's data. Put per-core assets in their
  own subdirectory, and add anything a core *writes* there to `SKIP_DIRS`/`MARKER_FILES` in
  `build.rs` so a debug run does not pack it back into `system.zip`.

## Conventions

- Unit tests live beside the code as a `mod tests` declared out of line: a `tests/` directory next
  to the source file, holding one `<module>_tests.rs` per module, pulled in with
  `#[cfg(test)] #[path = "tests/<module>_tests.rs"] mod tests;` at the bottom of the file (see
  `src/emu_file.rs` → `src/tests/emu_file_tests.rs`). They are still inner modules, so `use super::*`
  and access to private items work as before.
- Bevy systems: `#![allow(clippy::too_many_arguments, clippy::type_complexity)]` is set in `main.rs`.
- Prefer `set_if_neq` in per-frame systems — change detection drives real work in `post_process.rs`.
- Backends must not block the frontend; anything slow goes on a worker thread or through `jobs.rs`.

## Docs worth reading before touching those areas

`docs/AMIBERRY.md` (Amiga core options/WHDLoad), `docs/PCEM.md` and `PICO8.md` (the two
non-buildbot cores), `docs/flags.md` (core option reference tables), `docs/NOTES.md` (design
scratchpad for the loading pipeline), `docs/TODO.md` and `AI_TASKS.md` (open work), `CHANGELOG.md`.

## Releases

`dist` builds Linux/Windows/macOS artifacts when a `v<version>` tag is pushed. The tag must match
`version` in `Cargo.toml` exactly, prerelease suffix included. See the RELEASE section of `README.md`.
