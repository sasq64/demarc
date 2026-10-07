# Changelog

All notable changes to demarc will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),

## [1.8.0] - 2026-10-07

### Added

- **Navigator**: Path-based browsing of the database with a root menu, party drill-down, a Categories entry, compo placement sorting, back/forward history that restores selection and search text, and Next/Prev scoped to the launching list (`0a448fb`, `bac8953`, `bd45141`, `b4abef1`, `dabc08f`, `0a74d32`, `8487a35`, `7cf8347`).
- **Favorites and Playlists**: Named lists of releases persisted under the config dir, with a built-in Favorites list on the `H` hotkey, a picker to add/remove a release from any playlist, and the download URL a release actually used remembered (`4f61157`, `b32e4b7`, `af09388`).
- **Multiple Databases**: Several named databases can be loaded at once, each with its own file list and navigator root entry (`0ed6abe`).
- **DJ Mode**: `--dj-mode` gives the next release its own cue window and brings it over by hand with an equal-power audio crossfade; `--cross-fade-activity` holds the automatic fade until the hidden screen is active (`ad3f2b1`, `760c64a`).
- **PSP**: Added PSP system support (`59eac6d`).
- **86Box**: DOS machine configs run on 86Box, replacing PCem (`45ff299`, `ee3ba55`, `c14cbfc`).
- **Native Windows Releases**: Windows demos run natively when demarc itself runs on Windows (`9af36c0`, `f40dbca`).
- **Out-of-Process Cores**: `--proc` runs libretro cores in a separate process (`f464510`).
- **Picker Icons**: Per-row platform, category and party icons in the picker and navigator (`5a567ff`, `44880f3`, `669e73d`, `4dfa716`).
- **Award Display**: Per-award nominee/winner markers replace the coarse CDC/viewing-tip fields (`43c2c10`, `549744a`).
- **Tracker Music**: XM, S3M and IT play in the Fasttracker II core (`043d13e`).
- **Protracker Music**: MOD,STK...  play in the Protracker core
- **C64 REU**: Detect REU from the category tag and load standalone `.reu` images (`e1951a9`).
- **Override Enhancements**: bsdiff patches, fixups keyed on db and id rather than only demozoo, and many new per-release fixups (`a10ba56`, `84c504d`, `fb0a7fa`, `f392ab0`).
- **Wine Options**: Per-release CPU count cap, Mesa GLSL version override, opt-in `EmulateModeset`, a wine-native `winmm.dll` for Crinkler range imports and a DirectComposition shim (`d7128e2`, `af024dd`, `7c67c1c`, `dbaeaa0`, `13508fa`).
- **`--window=WxH`**: `--window` optionally takes a size, and the settings dialog gains CRT/downsample limits and more resolutions (`cd703a1`).
- **Remote Control**: `load_demo(id)` jumps to a db entry by id (`8f04242`).
- **7z Archives**: Added 7z support via `sevenz-rust2` (`e07d1da`).
- **More Mirrors**: scene.org no-http and se2-http mirrors, and a case-fixed filename retry for amigascne downloads (`2b7e6f3`, `fe4f7c4`).
- **Android Port**: A stripped-down C64 player (`minimarc`) builds for arm64 with audio, touch input and a Gradle project (`1e7ff57`, `9a47b71`, `35af9c6`, `81460aa`, `6434614`, `5733320`).

### Changed

- **Cargo Workspace**: Split the libretro layer, loading pipeline, librashader backend and egui UI into the `retro-core`, `newsys`, `retroarc` and `retro-ui` crates (`8c78bb2`, `e3ab394`, `f625e63`, `2466493`).
- **Default Sort**: `--sort=rank` is now the default, with rating used when the pouet score is missing (`12b2652`, `e3b2858`, `46c877a`).
- **Off-Main-Thread Core Lifecycle**: Core creation and teardown, including the outgoing cross-fade core, no longer block the main thread (`c12839b`, `1dd4d4b`).
- **Loading Pipeline**: Extracted into a `LoadingPlugin`, with loads routed through a `LoadFile` message and startup driven by an `AppState` instead of frame delays (`d3d1807`, `1b38fd2`, `08e64b6`, `5e49b87`).
- **Frame Upload**: Emulator frames are uploaded to the GPU directly from the render world, and screen change detection uses a diff-based activity measure instead of a frame hash (`ea9f841`, `f9175cc`, `8861752`).
- **Video Delay**: Video is delayed by a few frames through a shared-frame queue (`4e1fa3d`).
- **Windows Releases Without Wine**: Windows-only db entries are skipped when wine is unavailable, and Vulkan is preferred when demarc runs under wine (`d3f1b75`).
- **Double-Packed Archives**: Re-unpacking of archives inside archives is no longer limited to C64 releases (`e44c82f`, `413c94b`).
- **Fast Load**: Now a dynamic global setting instead of a per-system constructor field (`47b33f1`).
- **Setup Dialogs**: `autodlg` shows hidden setup dialogs, clicks default buttons directly and keeps the dialogs out of the captured session (`ecef47d`, `d886b65`).

### Fixed

- **Concurrent Core Loads**: Serialize core loading to avoid a race on libc statics shared between duplicated cores (`ac9f41e`).
- **Shader Texture Limits**: Clamp librashader intermediate textures to device limits and skip passes that would exceed them (`7ebf4ac`, `234bd82`).
- **PlayStation MODE2/2336 Discs**: Re-sector 2336-byte tracks to the raw 2352 layout the cores read (`5c35fba`).
- **Wine Audio Crash**: Fixed a winmm `WAVEHDR` crash when apps reuse the header after writing it (`ef0bd63`).
- **Wine Setup Dialogs**: Patch zero-size resource directories so setup dialogs work (`87e47a4`).
- **Multi-Disk Sets**: Fall back to individual disks when a multi-disk set fails (`6e7dfe6`).
- **Amiga Boot Filenames**: Quote the boot filename in the AmigaDOS startup-sequence (`417e387`).
- **Windows Launch Target**: Skip `redist/` directories when picking the exe and match more resolution-from-name separators, including 320x240 (`a1334d7`, `5b5e9e4`, `1e9fc52`).
- **Pause Handling**: Pause no longer counts against the idle timeout or `--max-time` (`275b231`).
- **Cross Fade**: Show the info text and start `--max-time` when a cross-fade load takes over (`f12a753`, `ccc8d13`).
- **Frame Alpha**: Force alpha opaque when repacking XRGB8888 frames (`e6a3563`).
- **Misc**: Empty db fields treated as absent, `.rom` recognized for Atari 2600, a piped stdin db skipped under `--remote-control`, and the picker scroll area pinned to full height on the first frame (`9d37ab4`, `124db58`, `f97b804`, `c54d344`).

## [1.7.0] - 2026-09-15

Includes the changes from the unreleased 1.6.0.

### Added

- **Windows Demos**: Windows releases run under wine inside a gamescope capture session, so they get shaders, grid layout and screenshots like any other system. Each session is sandboxed in its own throwaway prefix, with `--check-wine` and a prefix setup script (`b636d4c`, `c084112`, `6627ea0`, `3d7cfeb`, `e1482b3`, `86a7bc3`).
- **Web Releases**: `WebSystem` shows HTML/JS releases through the same gamescope core (`c084112`).
- **DOS**: PC/DOS support through PCem and DOSBox Pure, with GUS support, aspect-correction defaults and automatic DOS/4GW placement (`04ec7a2`, `e622af0`, `f1274e0`, `6fdec29`, `3787d1c`).
- **Amiberry Core**: Amiga releases can run on the Amiberry libretro core, with WHDLoad boot support, JIT for AGA demos and `$DEMARC_CORE_DIR` to use local cores (`2df227f`, `06b1ac6`, `326cc35`, `bc48ff2`).
- **New Systems**: Pico-8 via fake-08 and Plus4/C16 via yape (`eec87e9`, `1e948c6`, `00ffd7a`).
- **Per-Release Overrides**: `overrides.toml` fixes up releases the db gets wrong — which download and file to boot, meta/core options, AmigaDOS assigns, patches and scripted key events — with `--boot-file` for local content (`8eb44d1`, `fb10fc4`, `2a7e135`, `0fd26ba`, `7a789ae`).
- **Cross Fade**: `--cross-fade` loads the next release into a spare off-screen emulator and fades it in (`42270e9`, `5df1a55`, `8b0de87`).
- **Settings Dialog**: Runtime settings dialog with nested sections and a Wine panel (`f804220`, `515eef1`, `12f4677`).
- **Shader Dialog**: Shader collection and Mega Bezel preset pickers driven by `shaders.toml`, with editable parameters (`a2f7ad8`, `95983b0`, `ebbe051`, `d33556d`, `d27d797`).
- **Remote Control and Headless**: `--remote-control` drives demarc from a Luau script and `--headless` runs offscreen (`9178e86`, `cf23fb1`, `ef45df8`).
- **Sorting and Limits**: pouet.net rank in db entries with `--sort rank/random/date`, plus `--limit` and `--skip-count` (`e2fc182`, `09aac22`, `66ba599`, `947b348`).
- **Awards**: Pouet coup-de-coeur and viewing-tip awards shown on file list rows (`0bce775`).
- **Download URL Picker**: Shift+Enter picks among an entry's download URLs (`b1436d2`).
- **Amiga `--unadf`**: Boot single-disk demos as a hard drive, including DMS archives (`e1b9ec8`, `e6d6605`).
- **libretro VFS**: Implemented the VFS interface, fixing Stella ROM loading (`4fabd57`).
- **Border Cropping**: Crop borders using the core-reported used frame size (`872e4d9`, `98d5923`).
- **Misc**: Palette TIFF images, `.v2m` music files, a whole-screen screenshot on Shift+T, and `.txt`/`.txt.gz` files auto-detected as databases (`404fccc`, `cb29ceb`, `0a12914`, `4cbaad9`).

### Changed

- **Cache Budgets**: `FileCache` gains size-banded budgets, a user-editable `.limit` file and age expiry for cores (`b3d60fd`).
- **Release Directories**: Amiga and Atari ST release directories load as a single hard-drive entry, and command line arguments are only collected recursively when asked (`c0928cc`, `6ae31cd`).
- **Amiga Defaults**: Model defaults to A500, floppy speed is pinned, `requires-1mb-fastmem` is honoured and demozoo AGA demos only load as AGA when the date is new (`9f5d799`, `5ea46a6`, `67d6636`, `bf3e6ec`).
- **Launch Target Ranking**: Prefer non-Windows executables, an exe over a bat beside it and 8.3-named programs; `.bat` files are no longer claimed as DOS programs (`4982c49`, `a1cc5ec`, `489e204`, `4be153f`).
- **Shader Loading**: Shader chains build off the render thread with parallel pass compilation (`95983b0`, `16a08df`).
- **Unpacking**: Releases unpack off the main thread (`26a32ef`).
- **Grid View**: Windows entries are excluded from the grid (`2d4c631`).
- **Atari ST**: Hatari's LED status display is hidden, `hatari_ramsize` is renamed `hatari_memory_size` and the aspect ratio tweak is disabled (`ab660d5`, `521fab1`, `46ed5ae`).
- **HUD**: Shows remaining download bytes instead of the download count (`41d2417`).
- **Performance**: Vectorized XRGB8888 frame conversion and skipped `system.zip` rebuilds when inputs are unchanged (`323c53f`, `7e23be1`).
- **Refactoring**: Split `retro_emu` into a backend-agnostic trait and threaded worker, extracted config/system_dir/pixels modules and moved tests out of line (`7beb7d3`, `fd8e2ad`, `bce8f99`, `6ee0286`).
- **Removed**: The `--gus` flag and the `profile` feature (`09aac22`, `0b69ba4`).

### Fixed

- **C64 SID Model**: Use the 8580 SID by default (`2f3bbf2`).
- **Core Unload**: Call `retro_unload_game` and add a shutdown timeout (`073e6f2`).
- **Dead Downloads**: Fall back to other release URLs when a disk set is dead (`f969f33`).
- **Amiga Executables**: Accept hunks that run into the next without `HUNK_END`, strip embedded comments from LHA entry names and fix AGA model detection (`6ecf5a8`, `9ea7022`, `5ea46a6`).
- **Disk Sorting**: Fixed `sort_disks` when every name claims the same slot (`dbe9b46`).
- **File Picker**: Fixed stuck modifier keys (`199f113`).
- **Warp Indicator**: Taken down when the skip actually finishes (`e5c4565`).
- **C64 Fast Load**: Only send F1+Return when fast-loading (`b8b85e9`).
- **Windows Paths**: Strip the `\\?\` verbatim-path prefix before handing paths to cores (`797cf80`).
- **Widescreen Detection**: Detected from the monitor rather than the window (`89dc7bd`).
- **Audio**: Raised audio buffer bounds (`1307d44`).

## [1.5.0] - 2026-08-22

### Added

- **New `newsys` Loading Pipeline**: Replaced the old file-detection and load path with a  generic `System` trait, covering archive/disk handling, loading and per-system configuration (`b8eb1c7`, `eacf457`, `b5241b6`, `9e85034`).
- **Many New Systems**: Amstrad, Atari 2600, Atari XL, GBA, Megadrive, Sinclair ZX Spectrum, SNES and TIC-80 (`cabf235`, `1381027`); PlayStation with m3u multi-disc handling (`a9ecf2c`); Atari ST and the image viewer (`fbc1d29`); bare music files (`7f43f3a`).
- **Neo Geo CD**: Added Neo Geo CD support with the disc-image code shared with PSX, plus the Neo Geo BIOS (`78f505a`, `3aeae1b`).
- **Asynchronous Loading**: Demos load on a background task so downloads no longer freeze the emulator (`a6889dc`).
- **Luau Music Visualization**: User modifiable LUA script for music visualization (`353af2e`, `9f9baa3`).
- **egui Frontend**: Ported HUD toasts, the text list, file picker search/select/info and the hotkey list to egui, behind a reusable fuzzy-list widget (`ecebb92`, `b06b220`, `c9abe6b`, `c51c909`, `adb2009`, `f47c414`).
- **New Image Formats**: ZX Spectrum screens, PCX and TGA (`21da2aa`); NEOchrome, CrackArt and KID (`18a4869`); DEGAS via `ImageSystem` (`75ddf43`); Amiga super-hires with AGA vs OCS/ECS palette detection (`b1b7adc`).
- **Image Format Descriptions**: Report format details — including truecolour images by distinct colour count — for the frontend info display (`a659d18`, `ec16193`, `702b169`).
- **Download Resilience**: Retry downloads across mirrors and remember the one that works, falling back to the next release URL on failure, with an in-flight download counter in the HUD (`f7fad43`, `938d8ee`, `b0c94a6`).
- **Per-View Focus**: Avoid work in unfocused/invisible grid cells. (`6f6ce3b`).
- **DREZ Downsampling**: Downsample minified views, configurable with `--downsample` (`93adaa6`).
- **Retro Replay Autoboot**: Autoboot the Retro Replay cart for C64 disk/m3u loads under `--fast-load`, with autoboot and console input mode wired through the `System` trait (`b151267`, `39c0604`, `9e32254`).
- **Beetle PSX Core**: Support `mednafen_psx` for PSX under `--grid` (`7768568`).
- **Wayland Idle Inhibit**: Prefer Wayland idle-inhibit for screensaver suppression on Linux (`b444ba1`).
- **Shared `FileCache`**: One cache implementation reused across all disc and download caches (`da30902`).
- **Release Artifacts**: Publish the demo databases as release artifacts and ship CSDB/Demozoo launcher `.BAT` files in the Windows zip (`8fa8a70`, `a1412b6`).

### Changed

- **Tags Reworked**: Renamed tags to meta with a real tag lookup, described entries from tags instead of `SystemType`, and simplified db parsing so all named fields land in tags — including year extraction from the date tag (`57ca2e4`, `a62c554`, `fdb027d`, `0df2154`).
- **Single Shared Camera**: Composite all emulator views through one camera and drop the passthrough slangp chain in favour of compositing the framebuffer directly (`05fe7b4`, `202e4d1`).
- **`--crt-limit`**: Default lowered to 1.0 (`e0432f2`).
- **Atari ST Configuration**: Hatari is configured from content tags, releases from 1994 onward default to STE with 4 MB, and an ST-specific aspect-ratio tweak was added (`5d535b3`, `63c8931`, `ce4c3d4`).
- **Logging via `tracing`**: Replaced ad-hoc `println` debugging with `tracing` macros and demoted per-file walk, system-probe and frame drop/duplicate logs to trace (`60654c9`, `47e49a8`, `373f41e`, `e0fa8d9`).
- **HUD Presentation**: Drop shadows render behind the text (`21a5692`, `ac64bf1`, `868c90b`).
- **Mirror Resolution**: Resolve db link classes (`SceneOrgFile`, `ModlandFile`, …) to mirror URLs (`450ab97`).
- **Image Ordering**: Better image sorting, multi-disk image sorting, and jpg/jpeg screenshots ranked below other true colour formats (`cffa732`, `3ceb7bd`, `7f4c4d4`).
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
