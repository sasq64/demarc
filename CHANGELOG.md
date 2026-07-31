# Changelog

All notable changes to demarc will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

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
