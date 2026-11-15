# minimarc — the Android app

A stripped-down demarc for browsing and watching C64 demos on a phone: it reads
`csdb.txt`, shows demarc's picker over the navigator tree, downloads the picked
release and runs it in the VICE libretro core. No Bevy, themes, slangp shaders,
grid or cross-fade. The same crate runs on the desktop.

The repo-root `CLAUDE.md` applies here too (read `docs/LEARNINGS.md` first,
record mistakes there, small changes, short comments, test layout).
`docs/ANDROID.md` has the app's design at the top; everything in it below
"The rest of this file is the log" is history, and its paths are stale.

## Where things are

```
crates/minimarc/src/player.rs   window, wgpu 30, lottes CRT pass, egui pass, audio, keys,
                                touch, android_main, asset unpacking
crates/minimarc/src/ui.rs       Ui: picker + HUD + Navigator; draw() returns the picked EmuFile
crates/minimarc/src/load.rs     Loader threads: db parse (CATEGORIES filter), download + unpack
crates/navigator                the tree of lists, shared with demarc (src/navigator.rs wraps it)
crates/retro-ui                 the egui picker/HUD, shared with demarc
crates/release-io               fetch, cache, unpack, WorkFile, c64::prepare
crates/database                 db parsing, EmuFile; used with default-features = false
android/                        Gradle project: NativeActivity, no Java code
scripts/run-android.sh          the whole build-install-launch loop
```

`retro-ui`, `navigator`, `release-io` and `database` are shared with demarc: a
change there must keep demarc's behaviour, so run the whole test suite.

## Build, run, test

```sh
scripts/run-android.sh              # cargo ndk + gradle + adb install + launch (~1 min)
cargo nextest run --workspace       # ~3 s
cargo run --profile release-fast -p minimarc -- demodb/csdb.txt   # desktop; opens a window
```

Do not open the desktop window unasked (root rule: windows and audio). The
phone is the test target; it is a Unihertz Titan 2 (1076x1200, Android 16,
physical keyboard) that shows up in `adb devices` when attached.

The cross-compile gate, when touching dependencies of the shared crates:

```sh
cargo ndk -t arm64-v8a -P 26 check -p release-io -p navigator
cargo ndk -t arm64-v8a -P 26 check -p database --no-default-features
```

## Looking at the phone

```sh
adb exec-out screencap -p > <scratchpad>/shot.png      # then Read the PNG
adb logcat -d | grep -E " demarc *:" | grep -v spurious | tail -30
adb shell input tap X Y
adb shell input swipe X1 Y1 X2 Y2 150
adb shell input keyevent 4                             # Back
adb shell pidof com.demarc.c64
adb shell dumpsys activity activities | grep -m1 ResumedActivity
```

- Tracing and the core's own log both go to logcat tag `demarc`. A crash shows
  as `Fatal signal` / `DEBUG   :` lines, not under that tag.
- After `run-android.sh` returns, the splash is still up for a second or two
  and the db takes a few more to parse: a screenshot taken at once shows the
  launcher or an empty screen.
- A black screenshot usually means the phone went to sleep
  (`adb shell dumpsys power | grep mWakefulness`). It has a lock screen; ask
  the user to unlock rather than trying to get past it.
- The user is often using the phone at the same time. Keys and picks in the
  log that you did not send are theirs, and an `adb shell input` lands in
  whatever they are doing.

## How it behaves

- Starts in the picker with no core. A pick goes to a loader thread; the
  result replaces the core (`App::start`). Stale results are dropped.
- Picker open: all input goes to egui. Closed: keys go to the core.
- During a demo a sideways swipe is the next or previous release, a swipe up
  opens the key bar (`Ui::draw_keys`, `BAR_KEYS`) and a swipe down closes it.
  Its buttons are hit-tested in `App::touch`, not by egui.
- Escape or Back toggles the picker; a tap on a running demo opens it;
  Left/Right keys, or a sideways swipe over the picker (`nav_touch`), go back
  and forward in the tree.
- The picker has a button row under the info box: play picks the highlighted
  row, the floppy inserts the next disk. The db is sorted by rating on load.
- The screen is kept on for as long as the app is up (`android_main`).
- `UI_ZOOM` in `player.rs` is 2.0 on Android, 1.0 elsewhere. The picker's list
  height is `visible_rows` in `retro-ui/src/picker.rs`, which gives up rows
  when the zoom leaves no room (`CHROME_HEIGHT`).
- The C64 load path: fast-load cartridge `rr38ppal-auto.crt`, then F1 at core
  frame 50 and Return at 55 (`release_io::c64::FAST_LOAD_EVENTS`).

## Android facts that cost time

- Gradle gunzips `.gz` assets and drops the suffix; assets are shipped plain
  and `unzip -l` on the APK is the check.
- Back only reaches the app because the manifest sets
  `enableOnBackInvokedCallback="false"`; winit then reports it as the logical
  key `NamedKey::BrowserBack` with an `Unidentified` physical key.
- The core is loaded in place by soname from the APK's lib dir; nothing
  executable may be loaded from the data dir. Cores cannot be downloaded.
- Everything writable is under the app data dir: `TMPDIR`, the release cache
  (`release_io::set_cache_dir`) and the system dir.
- `egui-wgpu` 0.36 needs wgpu 30; the rest of the workspace is on 29 for Bevy.
  Only this crate names 30.
- `unarc-rs` does not build for bionic: no RAR or LHA on Android
  (`release-io/src/utils/plain.rs` does zip, gzip, 7z).
- `cargo ndk -o` copies every cdylib it builds into `jniLibs`; the script
  removes the `sevenz_rust2` one.

## State

Seen working on the phone: db load from the asset, the picker, picking a demo
(download, unpack, VICE up to the cartridge screen), Back toggling the picker,
swipe navigation, 2x text.

Not yet confirmed on the phone:

- that the picker fits the screen at 2x (title not clipped, info panel
  visible) — the phone locked before the check;
- a second demo after the first (core reload in place); if stale globals show
  up, keep the library loaded and use `retro_unload_game`/`retro_load_game`;
- sound, touch scrolling of the list, a demo running past the loader.

Known rough edges: the soft keyboard is only asked for after a tap on the
search box (`Ui::soft_keyboard`), and then covers the picker's buttons until
a tap elsewhere; no on-screen C64 keyboard; no
`overrides.toml`, playlists or next/previous; no audio drift control.

Nothing from this work is committed yet. Commit only when asked.
