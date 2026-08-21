# Changelog

All notable changes to demarc will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

## [1.5.0] - Unreleased

### Added

- **New `newsys` Loading Pipeline**: Replaced the old file-detection and load path with a `newsys` module built on a generic `System` trait, covering archive/disk handling, async loading and per-system configuration (`b8eb1c7`, `eacf457`, `b5241b6`, `9e85034`).
- **Many New Systems**: Amstrad, Atari 2600, Atari XL, GBA, Megadrive, Sinclair ZX Spectrum, SNES and TIC-80 (`cabf235`, `1381027`); PlayStation with m3u multi-disc handling (`a9ecf2c`); Atari ST and the image viewer (`fbc1d29`); bare music files (`7f43f3a`).
- **Neo Geo CD**: Added Neo Geo CD support with the disc-image code shared with PSX, plus the Neo Geo BIOS (`78f505a`, `3aeae1b`).
- **Asynchronous Loading**: Games load on a background task so downloads no longer freeze the emulator (`a6889dc`).
- **Luau Music Visualization**: Scripted visualizers with an API covering `noise()`, buffer types and more fonts (`353af2e`, `9f9baa3`).
- **egui Frontend**: Ported HUD toasts, the text list, file picker search/select/info and the hotkey list to egui, behind a reusable fuzzy-list widget (`ecebb92`, `b06b220`, `c9abe6b`, `c51c909`, `adb2009`, `f47c414`).
- **New Image Formats**: ZX Spectrum screens, PCX and TGA (`21da2aa`); NEOchrome, CrackArt and KID (`18a4869`); DEGAS via `ImageSystem` (`75ddf43`); Amiga super-hires with AGA vs OCS/ECS palette detection (`b1b7adc`).
- **Image Format Descriptions**: Report format details — including truecolour images by distinct colour count — for the frontend info display (`a659d18`, `ec16193`, `702b169`).
- **More Music Formats**: Extended the recognized extension set across tracker, Amiga and custom formats, including `.ma` and `.hipc` (`f96c858`, `6a3e2ac`, `0e8b50d`, `cc22d91`, `709eabc`).
- **Download Resilience**: Retry downloads across mirrors and remember the one that works, falling back to the next release URL on failure, with an in-flight download counter in the HUD (`f7fad43`, `938d8ee`, `b0c94a6`).
- **Per-View Focus**: Added per-view focus state driving grid maximize and idle music (`6f6ce3b`).
- **DREZ Downsampling**: Downsample minified views (`93adaa6`).
- **Retro Replay Autoboot**: Autoboot the Retro Replay cart for C64 disk/m3u loads under `--fast-load`, with autoboot and console input mode wired through the `System` trait (`b151267`, `39c0604`, `9e32254`).
- **Beetle PSX Core**: Support `mednafen_psx` for PSX under `--grid` (`7768568`).
- **Wayland Idle Inhibit**: Prefer Wayland idle-inhibit for screensaver suppression on Linux (`b444ba1`).
- **Shared `FileCache`**: One cache implementation reused across all disc and download caches (`da30902`).
- **Release Artifacts**: Publish the demo databases as release artifacts and ship CSDB/Demozoo launcher `.BAT` files in the Windows zip (`8fa8a70`, `a1412b6`).
- **Heading Alignment**: `heading_with_shadow` supports text alignment (`5263f16`).

### Changed

- **Tags Reworked**: Renamed tags to meta with a real tag lookup, described entries from tags instead of `SystemType`, and simplified db parsing so all named fields land in tags — including year extraction from the date tag (`57ca2e4`, `a62c554`, `fdb027d`, `0df2154`).
- **Single Shared Camera**: Composite all emulator views through one camera and drop the passthrough slangp chain in favour of compositing the framebuffer directly (`05fe7b4`, `202e4d1`).
- **`--downsample`**: Now a magnification threshold rather than a flat factor (`72a3f4d`).
- **`--crt-limit`**: Default lowered to 1.0 (`e0432f2`).
- **Atari ST Configuration**: Hatari is configured from content tags, releases from 1994 onward default to STE with 4 MB, and an ST-specific aspect-ratio tweak was added (`5d535b3`, `63c8931`, `ce4c3d4`).
- **Logging via `tracing`**: Replaced ad-hoc `println` debugging with `tracing` macros and demoted per-file walk, system-probe and frame drop/duplicate logs to trace (`60654c9`, `47e49a8`, `373f41e`, `e0fa8d9`).
- **HUD Presentation**: Drop shadows render behind the text instead of below it, the error location renders in red, and the year moved onto the group line in emulator info text (`21a5692`, `ac64bf1`, `868c90b`).
- **Mirror Resolution**: Resolve db link classes (`SceneOrgFile`, `ModlandFile`, …) to mirror URLs (`450ab97`).
- **Image Ordering**: Better image sorting, multi-disk image sorting, and jpg/jpeg screenshots ranked below other truecolour formats (`cffa732`, `3ceb7bd`, `7f4c4d4`).
- **Cleanup**: Removed the legacy pre-`newsys` pipeline, the superseded file-detection code, the `systems` module (folding `GameInfo`/info text into `Emulator`), the obsolete bevy-UI fuzzy list and unused `text_input` (`9e85034`, `273c66e`, `dd3cf02`, `eedf71d`, `5eae6ae`).

### Fixed

- **Amiga Executables**: Validate by parsing the hunk format, fix AMOS handling, and fix the 1997+ meta key and `copy_all` detection (`a51eaec`, `9c1ed96`, `73de6fc`, `f9ae1f9`).
- **ILBM Robustness**: Hardened chunk parsing against malformed/truncated files and let a square BMHD aspect override the mode-id guess (`44e0a99`, `81b87cd`).
- **Content-Based Detection**: PlayStation discs detected by content instead of filename/cue, `.atr` Atari disk images by header, standalone Atari XL binaries recognized, and false-positive tar detection in `is_archive` fixed (`0866945`, `3db94b0`, `07bc58f`, `e3b8a3d`).
- **Atari ST Loading**: Pick the boot program by name rather than size and scope hard-drive staging by release instead of directory (`29a054b`, `30455ef`).
- **Directory Scanning**: `scan_release_dir` no longer picks a screenshot folder over the demo folder (`ad04d37`).
- **TV Mode**: Max-time restart no longer re-triggers every frame (`e0bec65`).
- **Audio While Skipping**: Drain core audio while frames are skipped (`1b09d82`).
- **Unrecognized Files**: Report path details when no system recognizes a file (`e1aef30`).
- **Misc Loading**: Fixed floppy detection, image pause state and loader error handling (`f6dd2be`).
- **Release Build**: Fixed `release-fast` LTO setting causing link errors (`d57fdb7`).

## [1.4.0] - 2026-08-11

### Added

- **Music Playback via musix**: New `MusicEmu` backend plays SID, AHX, MOD and other tracker/chiptune formats through the `musix` crate.
- **Atari ST DEGAS Images**: Decode DEGAS and DEGAS Elite `.PI1`/`.PC1` stills into the same indexed-image path as ILBM, including Elite colour-cycling animation. (`196cdd7`).
- **C64 LNX/P00 Support**: Convert `.lnx` and `.p00` archives via `cbmconvert` (as with `.t64`) and recognize all three as C64 media (`6a13721`).
- **File Picker Info Panel**: The picker shows type/year, party, tags and source (path or truncated URL) for the highlighted entry, backed by a new `FuzzySource::get_info` hook (`328142c`).
- **Atari ST Hard Drive Loading**: Load Atari ST release directories as GEMDOS hard drives (`05dad3d`).
- **PS-X Bootable Images**: Wrap PS-X executables in a bootable disc image for `pcsx_rearmed` (`5e57f59`).
- **Bindgen Generation Script**: Regenerated `libretro.rs` with allowlisted bindgen and added a generation script (`48e15fe`, `858f714`).

### Changed

- **Fuzzy Search Limit**: Raised the `FuzzyList` default max results from 256 to 500,000 so unfiltered search can cover a full database (`9f17a06`).
- **STE/SID Tag Mapping**: Derive `vice_sid_extra`/`vice_sid_model` from `2sid`/`6581` db tags and Hatari machine type/RAM size from the `ste` tag; `--ste` now sets 4 MB of RAM instead of 2 MB (`6a13721`).
- **URL Rewrites Moved to demodb**: scene.org, modland, untergrund and SNDH mirror rewrites now happen when the database is generated instead of at download time (`70c923e`, `c96dd06`).
- **Keyboard Bindings**: Reworked the default keyboard-to-joypad bindings (`f1955ce`).
- **Multiview PSX Core**: Force the beetle PSX core for multiview cells (`565b796`).
- **Screenshot Chroma**: `pouet_shot` keeps 4:4:4 chroma until the byte budget forces 4:2:0 (`41e1e51`).
- **Thread Spinning**: Tamed OpenMP thread spinning in emulator cores (`338f59e`).
- **Cleanup**: Removed dead code and dropped blanket `dead_code` allows (`9210169`); dropped unused file picker constants (`bdf1c58`); renamed `Backend::frame_serial` to `frame_hash` (`ed93df6`).
- **Docs**: Updated install instructions (`5b91640`).

### Fixed

- **List Navigation Key Repeat**: Handle picker navigation from `KeyboardInput` events instead of `ButtonInput` polling, so held keys repeat reliably (`e52e76c`).
- **Oscilloscope Sync**: Delay the scope trace by the audio output latency (~140 ms) so the trace matches what is heard (`78659b5`).
- **Audio Sum Panic**: Cast to `i32` before `abs()` to avoid a panic on `i16::MIN` samples (`1cb2b75`).
- **Directory Scanning**: Skip dotfiles when scanning release directories (`196cdd7`).

## [1.3.1] - 2026-07-31

### Added

- **Neo Geo AES Support**: Integrated the `geolith` libretro core, `.neo` file extension detection, overscan tags, and AES BIOS requirement (`2ace71d`).
- **Download Cache LRU Pruning**: Added automatic startup eviction of least-recently-used cached downloads when total cache size exceeds 500 MB (`e874cda`).
- **TV Mode & Idle Detection**: Added `--tv-mode` flag for looping playlists with automatic skip of unloadable files or idle/still screens (`--idle-timeout`) (`8669230`).
- **Bevy optimizations**: Turn off unused features, avoid unnecessary texture uploads.

### Changed

- **Audio Subsystem**: Upgraded `cpal` to `0.17.1`.
- **Repository Cleanup**: Moved database files and collection scripts to external repository (`7f936d9`).

## [1.3.1-rc.1] - 2026-07-30

### Added

- **cargo-dist Release Pipeline**: Integrated `cargo-dist` CI workflow and `cargo binstall` metadata for automated multi-platform binary releases (Linux, Windows, macOS) (`75d46ca`).
- **Generic Image Viewer Support**: Added Gfx system type supporting `.gif`, `.png`, `.bmp`, `.jpg`, and `.jpeg` image formats via `ImageEmu` (`27d1a6e`).
- **Repeatable DB Filters**: Supported combining multiple `-I/--include` filters (AND logic) and multiple `-X/--exclude` filters (OR logic) (`1e0afa8`).

### Fixed

- **Directory Auto-Selection**: Prioritized executable demo/game files over static screenshots when scanning release directories (`8353809`).
- **Audio State Reset**: Fixed false underrun detection when switching files by clearing `audio_seen` flag on load (`99e6ac3`).

## [1.3.0] - 2026-07-28

### Added

- **Unified Archive Extraction**: Replaced separate zip/lha extractors with `unarc-rs`, adding support for `.7z`, `.rar`, `.tar`, `.gz`, `.bz2`, and Unix `.Z` archives (`d4ee6ee`).
- **FTP Download Support**: Added `ftp://` scheme support via `suppaftp` with manual HTTP-to-FTP redirect handling and scene.org mirror optimization (`116475e`).
- **Fast Trigram Fuzzy Search**: Added trigram-indexed candidate filtering using `nucleo-matcher` for fast searching across large file databases (`a743946`).
- **Multi-Disk Archive Grouping**: Automatically fetch and extract all disk images for multi-disk release URL sets (`e8bc9a5`).
- **Database Enhancements**: Supported database comments, named fields, platform/header tags, semicolon-separated URLs, and stdin piping (`c0f5c38`, `e5ad296`, `7bd18cd`).
- **Embedded CBM Convert**: Vendored `cbmconvert` with Rust FFI wrapper for automatic `.t64` conversion (`968114e`).

### Fixed

- **PSX Header Patching**: Added header patching for truncated/undercounted PSX executable text section headers before loading (`61d221d`, `084e600`, `0b905db`).
- **GBA ROM Detection**: Added header inspection to identify GBA ROMs with blanked Nintendo logos (`f8f2cfa`).
- **Cache Collision Fix**: Keyed download cache directories on full URL SHA-256 hash instead of filename alone (`2d6fc6e`).
- **UI & HUD Enhancements**: Render non-ASCII input characters, clip overlong text list rows, and classify load failures into descriptive HUD messages (`1ca00d2`, `968114e`, `63d17a3`).
