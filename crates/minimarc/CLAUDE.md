# minimarc — the Android app

A stripped-down demarc for browsing and watching C64 and Amiga demos on a
phone: it reads `csdb.txt` and `demozoo-amiga.txt`, shows demarc's picker over
the navigator tree, downloads the picked release and runs it in the VICE
or amiberry libretro core. No Bevy, themes, slangp shaders,
grid or cross-fade. The same crate runs on the desktop.

The repo-root `CLAUDE.md` applies here too (read `docs/LEARNINGS.md` first,
record mistakes there, small changes, short comments, test layout).
`docs/ANDROID.md` has the app's design at the top; everything in it below
"The rest of this file is the log" is history, and its paths are stale.

The crate also runs on iOS: `scripts/run-ios.sh` on a Mac, see `docs/iOS.md`.

## Where things are

```
crates/minimarc/src/player.rs   window, wgpu 30, CRT pass, egui pass, keys, touch,
                                android_main, asset unpacking
crates/minimarc/src/crt.wgsl    minimarc's own cut-down lottes: one fragment entry point
crates/minimarc/src/web.rs      the release page: JNI call to MiniActivity.openWeb, its Open button
crates/minimarc/src/ui.rs       Ui: picker + HUD + Navigator; draw() returns the picked EmuFile
crates/minimarc/src/load.rs     Loader threads: db parse (CATEGORIES filter), download + unpack,
                                overrides.toml; `Platform` (C64 or Amiga) picks prepare and core
crates/minimarc/src/amiga.rs    Kickstarts out of Amiga Forever Essentials' APK, amiga.zip
crates/retro-core/src/audio.rs  AudioSink (cpal, resampler, drift control), shared with demarc
crates/navigator                the tree of lists, shared with demarc (src/navigator.rs wraps it)
crates/retro-ui                 the egui picker/HUD, shared with demarc
crates/release-io               fetch, cache, unpack, WorkFile, c64::prepare, amiga::prepare
crates/database                 db parsing, EmuFile; used with default-features = false
android/                        Gradle project: MiniActivity (NativeActivity + immersive mode)
scripts/run-android.sh          the whole build-install-launch loop
scripts/build-amiberry-android.sh  amiberry core for the APK (~4 min from clean)
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
adb shell pidof se.minnberg.demarc
adb shell dumpsys activity activities | grep -m1 ResumedActivity
```

- `adb shell setprop debug.minimarc.perf 1` before launch logs `perf:` lines
  every 5 s (core, ui and render ms per frame, avg/max); `MINIMARC_PERF` on
  the desktop.
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
- During a demo a swipe in any direction is that cursor key, and a long press
  (held past `SWIPE_TIME` without moving) or a tap opens or closes the key bar
  (`Ui::draw_keys`, `ui::ROWS`).
  Its buttons are hit-tested in `App::touch`, not by egui. Row one is
  F1/F3/F5/F7/Space/Return (held keys, `Button::key`), the soft keyboard and C; on an Amiga
  (`ui::AMIGA_ROWS`) the first four are mouse left/right (held), pointer mode
  (fingers off the bar move the mouse, a tap toggles the bar) and Esc; row two (`App::bar_button`) is disk flip (shows
  the disk number), stop (unloads the core, back to the picker), reset, pause,
  warp (while held, audio dropped), prev, next and back. `Button::closes_bar`
  says which buttons close the bar: C and all of row two. One touch gesture
  at a time (`player::Gesture`).
- C opens the release's page in `WebActivity`, a WebView: the db header's
  `release_url` (with `{id}`) and `release_css`, an APK asset from
  `system/web/` injected on load (`web.rs` calls `MiniActivity.openWeb` over
  JNI; Android only). Back finishes it; the demo plays on underneath.
  On a CSDb event/group or Demozoo party/group page whose name a db has releases under, an Open
  button in the header (a JS bridge to `nativeHasPath`/`nativeOpenPath` in
  `web.rs`) finishes it and opens Parties/<name> or Authors/<name>.
- Escape or Back toggles the picker;
  Left/Right keys, or a sideways swipe over the picker (`nav_touch`), go back
  and forward in the tree.
- The picker has a button column left of the info box: back closes the
  picker, play picks the highlighted row (an arrow when it is a directory,
  `Picker::dir_buttons`). The db is sorted by rating on load.
- Reset is VICE's `vice_reset = hard` (set in `c64::prepare`), so it does not
  autostart the release.
- The screen is kept on for as long as the app is up (`android_main`).
- In the background (or with the screen off) the core keeps running without
  a surface, so the sound goes on; `MiniActivity` starts `PlaybackService`, a
  media foreground service, in `onPause` and stops it in `onResume`, or the
  process would be frozen.
- Headset/media buttons (`media.rs`): a tap of play-pause or the headset
  button toggles pause after `DOUBLE_TAP`, a second tap within it skips to the
  next release; play and pause both toggle (a Bluetooth headset sends
  those, see `docs/LEARNINGS.md`). In the foreground they are winit keys; in the background
  `MiniActivity`'s `MediaSession` passes the keycode in over JNI
  (`nativeMediaKey`), and `App::step` drains them.
- The picker redraws on input and while egui asks for a repaint, not only on
  the core's frame tick.
- `UI_ZOOM` in `player.rs` is 2.0 on Android, 1.0 elsewhere. The UI scales
  with the window height up to `UI_MAX_HEIGHT` logical pixels, so it keeps its
  physical size on a tablet. `MiniActivity` pads the content view by the
  camera cutout, the rounded corners and (in portrait) the soft keyboard;
  `Gfx::safe_area` reads that back from `content_rect` as egui's safe-area
  insets. The picker's list
  height is `visible_rows` in `retro-ui/src/picker.rs`, which gives up rows
  when the zoom leaves no room (`CHROME_HEIGHT`).
- The C64 load path: fast-load cartridge `rr38ppal-auto.crt`, then F1 at core
  frame 50 and Return at 55 (`release_io::c64::FAST_LOAD_EVENTS`).
- Amiga: an entry whose platform starts with "Amiga" (demozoo-amiga.txt, cut
  out of demozoo.txt by `run-android.sh`) runs in amiberry, with
  `<data>/amiga` as its system dir. The buildbot has no Android amiberry:
  `scripts/build-amiberry-android.sh` cross-compiles `external/amiberry` in a
  copy under `target/`, and `run-android.sh` runs it when the .so is missing,
  so rerun it by hand after changing amiberry. The Kickstarts are decrypted at startup
  out of the installed Amiga Forever Essentials APK (`MiniActivity.packagePath`,
  a `<queries>` entry in the manifest); without it an Amiga pick is an error.
  `amiga.zip` (`ami13`, `amihdd`, `amiga/WHDBoot`, `amiga/floppy_sounds`) is unpacked over the data
  dir on every start. Pinch zoom is C64 only.
- The `[csdb.<id>]` and `[zoo.<id>]` entries of `overrides.toml` (an APK asset on Android, the
  system dir elsewhere) are applied in `load.rs`: download, meta, patches,
  boot file and key events. Other sections are skipped unparsed.
- A started release shows title, author and "<platform> <category>" for five seconds
  (`Ui::info`): top left of the window in portrait, bottom right of the
  picture in landscape (`Hud::info_place`).
- Sound goes through `retro_core::audio::AudioSink`; `App::step` steers the
  resample ratio by the ring fill (`AUDIO_BUF`) and runs an extra frame when
  the ring is nearly dry.

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
- `unarc-rs` (RAR, LHA) needs `release-io`'s `build.rs` Android link fixes;
  see `docs/ANDROID.md`.
- NativeActivity's `onDestroy` blocks until `android_main` returns, and winit
  0.30 drops Destroy, so `MiniActivity.onDestroy` kills the process first;
  without it a relaunch hangs on the splash screen.
- Swiping the app out of recents need not call the activity's `onDestroy`;
  `PlaybackService.onTaskRemoved` kills the process instead (it is only called
  without `stopWithTask`).
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
search box (`Ui::soft_keyboard`), until a tap elsewhere; no on-screen C64 keyboard; no playlists.

The shared audio sink, the overrides and the info text have only been run by
the tests and the cross-compile check, not on the phone.

Nothing from this work is committed yet. Commit only when asked.
