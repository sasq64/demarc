# Changelog

All notable changes to demarc will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

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
