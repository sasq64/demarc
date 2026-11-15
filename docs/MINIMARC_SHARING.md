# What minimarc and demarc share

Survey of the code minimarc (Android/desktop C64 player, `crates/minimarc`)
and demarc (Bevy desktop app, `src/` + desktop-only crates) have in common,
what is duplicated, and what demarc has that minimarc could pick up cheaply.
Written 2026-11-15 against the `minimarc` branch.

## 1. Shared today

| Crate | What minimarc uses from it | Notes |
|---|---|---|
| `retro-core` | `RetroCoreDirect`, `libretro` key constants, `find_core`/`dylib_name`, `system_dir`/`set_system_dir`, `backend::frame_bytes`, `backend::InputEvent`, `android::init_logging` | Uses the *direct* (same-thread) core, not `RetroCoreThreaded` that demarc runs every core on. `audio.rs`, `audio_recorder.rs`, `frame_import`, `pixels` are compiled in but unused by minimarc. |
| `release-io` | `EmuFile` path resolution (download into the content-addressed cache), `unpack_release`, `WorkFile`, `c64::prepare`, `c64::FAST_LOAD_EVENTS`, `set_cache_dir` | The whole C64 load logic is here, so both apps pick and boot C64 releases the same way. No `unarc-rs` on Android (zip/gzip/7z only). |
| `database` (no `desktop`) | `files::collect_db` + `DbFilter`, `emu_file::EmuFile`, `bytes_in_progress` | `overrides` and `modland` are behind `desktop` because they reach into `newsys`. |
| `navigator` | `Navigator`, `setup_navigator`, `ShowList`, `current_launch.step` (next/prev), `picker_sources::SHOW_URL` | demarc wraps it as a Bevy resource in `src/navigator.rs`. |
| `retro-ui` | `Picker`, `Hud`, `apply_style`, `set_scale`, `picker_font`, `TitleClick` | Not used by minimarc: `theme`, `shader_dialog`, `dialog`. |
| `system/` files | `system/font.ttf`, `system/shaders/lottes.wgsl` (with a stub for its `#import bevy_…` line), `rr38ppal-auto.crt` (shipped as an APK asset) | |

Everything else in minimarc is its own: winit event loop, wgpu 30 renderer
(frame upload, Lottes pass, egui pass), cpal audio, keyboard/touch handling,
the key bar, the loader threads, Android entry point and asset unpacking.

Desktop only, never reachable from minimarc: `newsys` (systems, wine, music,
images, DOS/Amiga/…), `retroarc` (librashader), `frameconv`, `rife`,
`flash-emu`, and everything under `src/` (Bevy).

## 2. Duplicated code that could be shared

Ordered by payoff / effort.

### 2.1 Audio output — minimarc re-implements `retro_core::audio`
**Done.** minimarc plays through `AudioSink`; `rate_adjust` and `smooth_fill`
live in `retro_core::audio` and both frontends call them. What follows is the
state before.

`minimarc/src/player.rs` `Audio` (≈90 lines) opens a cpal stream, picks a
config, resamples linearly into a ring. `retro_core::audio` already has all of
that, better:

- `pick_output_config` — minimarc copied this logic (including the WASAPI
  5512 Hz comment) inline.
- `AudioSink`/`AudioOut` — rubato resampler, ring, volume, dead-stream
  detection, `set_low_water`, and `set_adjust` for drift correction.

Switching minimarc to `AudioSink` removes the copy and gets drift control
(listed as a rough edge in `crates/minimarc/CLAUDE.md`) almost for free; the
missing piece is demarc's `rate_adjust` (`src/emulator.rs:1091`, six lines,
plus the fill smoothing at `:899`), which could move into `retro-core::audio`
next to `set_adjust` so both frontends call one function. cpal 0.17 and
rubato already cross-compile (retro-core is an Android crate).

### 2.2 Timed startup keys — minimarc re-implements event scheduling
minimarc keeps `startup_keys` and replays `FAST_LOAD_EVENTS` by hand with a
`KEY_HOLD` of 2 frames (`player.rs` `start`/`step`). `RetroCoreThreaded`
does this through `Backend::send_events` (`retro_emu/threaded.rs`
`play_due`), and demarc's C64 system just calls
`core.send_events(&FAST_LOAD_EVENTS)`. Options:

- move `play_due`'s queue into `RetroCoreDirect` so the direct core takes
  `send_events` too, or
- run minimarc on `RetroCoreThreaded` via `retro_emu::create_core` (Android is
  already handled there: `use_proc` falls back to a thread). That also brings
  `is_idle`/`is_silent`/`has_exited`, `set_audio_out` (audio pushed from the
  core thread) and latency control. Bigger change: frames arrive through the
  backend's update channel and minimarc's frame pacing would change, and the
  "one core at a time, loaded in place" Android constraint must still hold.

Either way minimarc would also honour `c64_fast_load` being disabled, which
it ignores today.

### 2.3 Keyboard map — two tables of the same thing
`src/emulator.rs` `build_keycode_map` (Bevy `KeyCode` → `retro_key`) and
`minimarc/src/player.rs` `retro_key` (winit `KeyCode` → `retro_key`) are the
same table: Bevy's `KeyCode` variants have winit's names. A name-keyed table
in `retro-core` (`"KeyA" → RETROK_a`, …) would serve both, plus
`Emulator::resolve_key_name`, which already matches by `format!("{k:?}")`
and is what `overrides.toml` key names go through (`KeyResolver`). Each
frontend keeps a one-line `Debug`-name lookup or a small `From` match. The
C64-specific `alt_key`/`alt_symbol` stay in minimarc.

### 2.4 `overrides.toml` — only a few newsys references keep it desktop-only
**Done.** `database::overrides` builds without `desktop`,
`release_io::apply_override` is the system-free part of applying one, and
minimarc reads the `[csdb.…]` entries (`overrides::load_default_for`). What
follows is the state before.

`database/src/overrides.rs` is gated by `desktop` but uses from `newsys`
only: `backend::InputEvent` and `libretro::retro_key` (both really in
`retro-core`), `system_dir` (where patch files live) and two string constants
`wine::META_GLSL_PATCHES` / `GLSL_PATCH_SEP`. Pointing the first two at
`retro-core`, moving the two constants into `release-io` (or `database`), and
taking the system dir from `retro_core::system_dir` would let it build
without `desktop` (`qbsdiff`, `toml`, `serde` are pure Rust). Applying an
override (`apply_override`, `apply_patches`, `write_patch`, `apply_bsdiff`
in `newsys/src/newsys.rs:168-300`) has no system dependency except
`amiga::apply_fast`, so the generic part could move to `release-io` next to
`unpack_release`. Today only 2 entries in `system/overrides.toml` are csdb
ones, so the value for minimarc is mostly future-proofing — rank this below
2.1–2.3.

### 2.5 Playlists/favorites — Bevy-free except one derive
`src/playlists.rs` (`Playlists`, `PlaylistEntry`, `PlaylistPicker`) touches
Bevy only through `#[derive(Resource)]`. The `navigator` crate already knows
about playlists (`FAVORITES`, `PLAYLIST_MENU`, `PLAYLIST_ROOT_MENU`,
`playlists: HashSet<String>`). Moving the file into `navigator` (or
`database`) and wrapping it in a newtype resource in demarc, the way
`src/navigator.rs` wraps `Navigator`, makes it available to minimarc. It
needs `serde` + a TOML/JSON writer, both fine on Android; the save dir would
be the app data dir there.

### 2.6 Smaller overlaps
- `fit_rect` / uv scaling in minimarc vs demarc's `post_process/geometry.rs`:
  different enough (demarc has scale modes, border, grid) that sharing is not
  worth it.
- Lottes shader: already one file; the `BEVY_IMPORT` stub in minimarc is the
  only seam. Fine as is.
- `load.rs` `CATEGORIES` regex and the rating sort are minimarc policy; demarc
  has its own db filters. Leave separate.

## 3. demarc features that would be cheap in minimarc

Cheap = mostly already in a shared crate, or a few dozen lines in minimarc.

1. **Auto-advance (idle timeout / TV mode).** demarc moves to the next
   release when the core exits or has been idle (`is_silent &&
   !screen_changed`) for `idle_timeout` seconds (`src/frontend.rs:596-616`).
   minimarc already has `Ui::step(1)` for next. With the threaded core
   (2.2) `is_idle` is free; with the direct core it needs a silence check
   (`retro_core::audio::is_silent` on the samples it already pulls) plus a
   frame-diff. Good fit for a phone left playing.
2. **Audio drift control.** See 2.1 — falls out of using `AudioSink`.
3. **Favorites.** With 2.5 done, a heart button in the picker's button row
   (`picker.buttons`) toggling the current release, and the navigator's
   existing playlist root list shows them.
4. **Skip bad releases.** demarc's "skip bad files" (`config.rs:294`): on a
   load error, step to the next entry instead of reopening the picker. In
   minimarc that is a branch in `poll_loader` when the failed load came from
   a swipe/auto-advance.
5. **Reset.** `RetroCoreDirect::reset` exists; one more key-bar button.
6. **REU releases.** `c64::prepare(file, reu)` already takes the flag that
   demarc's `--reu` sets; minimarc hard-codes `false` while it lists
   "REU Release" in `CATEGORIES`. Passing `true` for that category is a
   one-liner (needs checking that VICE's REU option is set by `prepare`'s
   meta).
7. **Screenshot.** The frame is already in `with_frame`; `image` (workspace
   dep, pure Rust) can write a PNG into the app's data/Pictures dir. Low
   value on a phone, but very cheap.
8. **Pause.** Skip `core.run()` and stop audio when paused (also worth doing
   in `suspended`, which today only drops the surface).
9. **Volume.** `retro_core::audio::Volume` comes with `AudioSink`.

Not cheap / not worth it: themes and shader backgrounds (wgpu 30 vs Bevy
shader painter), slangp shaders (librashader is desktop-only), grid and
cross-fade, DJ mode, frame conversion, remote control (Luau + Bevy), audio
recording (MP3 needs LAME from C; WAV via `hound` would work if wanted).

## 4. Suggested order

1. 2.1 audio → `AudioSink` + move `rate_adjust` into `retro-core` (fixes a
   known rough edge, deletes code).
2. 2.3 shared key-name table (deletes a duplicate table, no behaviour change).
3. 2.2 `send_events` on the direct core (smallest step), then feature 3.1
   auto-advance.
4. 2.5 playlists into `navigator`, then favorites in minimarc.
5. 2.4 overrides, when a csdb release actually needs one on the phone.

Each touches a shared crate, so `cargo nextest run --workspace` plus the
`cargo ndk … check` gate from `crates/minimarc/CLAUDE.md` after each.
