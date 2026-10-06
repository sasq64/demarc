# Android

The minimal one-window VICE player — `crates/retro-core/src/player.rs`, run by
`crates/retro-core/src/bin/minimarc.rs` on the desktop — as an Android app. It boots to the
BASIC banner on a device; touch input is what is missing.

```sh
scripts/run-android.sh --logcat
```

builds it, fetches the core if needed, installs and launches it.

## Starting position

The player is already shaped for it: winit + wgpu with no Bevy and no librashader, the window
and the wgpu surface are created in `resumed()` and dropped in `suspended()` (the Android
lifecycle), the frame texture is a plain `Rgba8Unorm` upload, and the device is requested
with `Limits::downlevel_defaults()` so a GLES 3 fallback is in reach. Letterboxing uses
the render-pass viewport instead of a border-clamp sampler, which is a feature GLES does
not always have.

The core is available and clean (checked):

- `https://buildbot.libretro.com/nightly/android/latest/arm64-v8a/vice_x64sc_libretro_android.so.zip`
  exists, as does the `armeabi-v7a` one.
- The arm64 core needs only `libdl`, `liblog`, `libc`, `libm` — no `libc++_shared`, no GL —
  so nothing else has to be shipped beside it.
- Its `LOAD` segments are 16 KB aligned, so it loads on Android 15+ devices with 16 KB
  pages. Worth re-checking for any other core: a 4 KB-aligned one will not load there.
- It is built against API 21, and by the same NDK (r29) installed here.

## The structural blocker: one package builds all of its dependencies — done

Cargo compiles every non-optional dependency of a package when building *any* target in
it, whether that target uses them or not. So `cargo ndk build --bin c64` used to ask the
whole demarc dependency set — Bevy, librashader, `musix` (C++), `mlua`/Luau (C++), resvg,
symphonia, ureq, suppaftp — plus `build.rs`'s vendored C (cbmconvert, ADFlib, xDMS) to
cross-compile to `aarch64-linux-android`, and it died in `unrar_sys` 0.5.8
(`vendor/unrar/ulinks.cpp:39: error: use of undeclared identifier 'lutimes'` — bionic has
`utimes` but not `lutimes`).

### Step 1 — split into a workspace (done)

The repo is now a workspace whose root is still the `demarc` package:

```
Cargo.toml           [workspace] members = ["crates/newsys", "crates/retro-core", "crates/retro-ui", "crates/retroarc"]
crates/retro-core/   lib: backend.rs, libretro.rs, pixels.rs, retro_emu/, find.rs, path.rs
                     (`strip_verbatim_prefix` — the one thing retro_emu wanted from utils.rs,
                     whose rest pulls unarc-rs), retro_log_shim.c in its build.rs, and the
                     player behind an optional `player` feature.
                     [lib] crate-type = ["rlib", "cdylib"]   (cdylib is what the APK loads)
                     deps: anyhow, libloading, tempfile, tracing, dirs, libc (unix)
crates/newsys/       everything between a file on disk and a Box<dyn Backend>
crates/retroarc/     the .slangp filter chains; the only crate that links librashader
crates/retro-ui/     the egui HUD, picker and dialogs, against a bare egui::Context
src/                 demarc: the Bevy app
```

`src/bin/minimarc.rs` is now a thin `main` over `retro_core::player`; the `#[path]` includes and
the `retro_emu/mod.rs` layout they forced are gone.

**This works.** Both of these pass:

```sh
cargo ndk -t arm64-v8a -P 26 check -p retro-core                     # the core layer alone
cargo ndk -t arm64-v8a -P 26 check -p retro-core --features player --lib
```

and the library actually builds:

```sh
cargo ndk -t arm64-v8a -P 26 -o android/app/src/main/jniLibs \
    build -p retro-core --features player --lib --release
```

giving an ~800 KB `libretro_core.so` that needs only `libdl`, `libandroid`, `liblog` and
`libc`, with 16 KB-aligned `LOAD` segments (so it loads on Android 15+). winit already
has its `android-native-activity` feature wired up for the Android target, which is what
pulls `android-activity` in.

Two rules keep this working, and both are easy to break by accident:

- **`retro-core` may not gain a dependency that does not build for bionic.** That is why
  `utils.rs` stayed behind in `newsys`.
- **Nothing under `crates/` may mention Bevy.**

### Step 2 — make the libretro layer Android-safe — done

Three concrete things in `retro_emu`, all now in place:

1. **Core duping must be skippable.** `RetroCoreDirect::new` copies the `.so` into a
   private temp dir and `dlopen`s the copy, so two instances get independent globals. On
   Android, loading native code from the app's writable data directory is exactly what the
   platform blocks (W^X). The core has to be `dlopen`ed from the APK's native library
   directory instead, by bare soname. Add a "load in place" path (an argument, or
   `#[cfg(target_os = "android")]`) that skips the copy — with one view there is nothing to
   dupe anyway. `RetroCoreDirect::new` now holds its temp dir as an `Option` and loads the
   core in place on Android.
2. **`mod process` has to go on Android.** `RetroCoreProcess` re-executes the current
   executable and uses `memfd_create`; an APK has no standalone executable to re-exec.
   Gated `#[cfg(all(unix, not(target_os = "android")))]`, as is the `use_proc` arm of
   `create_core`.
3. **Nothing may print to stdout.** Cores `printf` freely and demarc silences that with a
   `dup2` on fd 1; on Android stdout goes nowhere at all, so core logging has to reach
   logcat through `RETRO_ENVIRONMENT_GET_LOG_INTERFACE` (already implemented) with a
   tracing subscriber that writes to `__android_log_write`. That is
   `crates/retro-core/src/android.rs`: a `MakeWriter` mapping each event's level onto a
   logcat priority under the tag `demarc`, plus a panic hook, so
   `adb logcat demarc:V` shows both our tracing and the core's own log lines. No new
   dependency — liblog is already linked in through `android-activity`.

### Step 3 — the entry point — done

`android_main` is at the bottom of `player.rs`. `android-activity` declares it
`extern "Rust"`, not `extern "C"`:

```rust
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    crate::android::init_logging();
    if let Some(dir) = app.internal_data_path() {
        crate::set_system_dir(dir);
    }
    let event_loop = EventLoop::builder().with_android_app(app).build()?;
    event_loop.run_app(&mut App::new(load_core()?))?;
}
```

winit re-exports the `android_activity` API, so naming `AndroidApp` needs no extra
dependency. `AndroidApp::internal_data_path()` is the system/save directory to hand the
core — `dirs::cache_dir()` is not meaningful here — and it reaches `system_dir()` through
`find::set_system_dir`, a `OnceLock` override checked ahead of the environment.
`asset_manager()` is how any bundled `.d64`/`.prg` would be read.

`App::step` now returns early without a surface, so the emulator does not run while the
app is in the background.

### Step 4 — the APK — done

`android/` is a Gradle project shaped like the one in `../band`, with the Rust `cdylib` and
the core in `jniLibs` (which is gitignored — both `.so` files are build output):

```
android/
  settings.gradle.kts, build.gradle.kts, gradle.properties, gradlew
  app/build.gradle.kts
  app/src/main/AndroidManifest.xml
  app/src/main/res/values/strings.xml
  app/src/main/jniLibs/arm64-v8a/libretro_core.so           <- cargo ndk output
  app/src/main/jniLibs/arm64-v8a/libvice_x64sc_libretro.so  <- renamed buildbot core
```

With `android-native-activity` no Java is needed: `hasCode="false"`, and the manifest points
at `android.app.NativeActivity` with `android.app.lib_name` = `retro_core`. No orientation is
pinned — `configChanges` keeps the activity alive across a rotation and the player letterboxes
to whatever it is given, which is what `../band`'s README argues for.

The core keeps the name `find_core` builds, with a `lib` prefix
(`libvice_x64sc_libretro.so`): Gradle packages it, the loader puts it on the app's search
path, and `dlopen` by bare name then works. `find.rs`'s Android arm returns exactly that
name instead of searching `$DEMARC_CORE_DIR` and the demarc cache. Note the buildbot core's
`SONAME` is a bare `libretro.so` — fine for one core, but two cores loaded at once would
collide in the linker's namespace.

`scripts/run-android.sh` does the whole loop — fetch the core if it is missing, `cargo ndk`,
`gradlew`, `adb install`, launch, optionally `logcat`:

```sh
scripts/run-android.sh               # release
scripts/run-android.sh --debug
scripts/run-android.sh --logcat
```

It defaults to release because a debug wgpu build is both slow and enormous, and sets
`JAVA_HOME` to Android Studio's JDK 21 when the shell has none — AGP does not run on this
machine's JDK 26.

Measured on the Titan 2 (Android 16, arm64): a 10 MB release APK, wgpu on the GLES backend
("EGL says it can present to the window but not natively"), VICE reporting 384x272 at
50.12 fps, and ~55% of one core.

### Step 5 — input

Physical keys already work — a keyboard arrives as ordinary winit `KeyboardInput` and goes
straight through the existing `retro_key` table; `adb shell input text` reaches BASIC, though
injected keys are faster than the emulated keyboard matrix scans and some are dropped. Touch
does not work at all, and that is the real work:

- An on-screen C64 keyboard and/or a virtual joystick, drawn by the app. Everything on
  screen is ours already, so this is one more textured quad plus hit-testing on
  `WindowEvent::Touch`, and it avoids the IME entirely. This is how RetroArch does it and
  it is the recommended route.
- The alternative, `AndroidApp::show_soft_input()`, gets the system keyboard up but under
  `NativeActivity` text comes back as `KeyEvent`s of limited fidelity; `GameActivity`
  (winit's `android-game-activity` feature) handles IME text properly but needs a Java/
  Kotlin shell. Only worth it if typing BASIC on the system keyboard matters.

Also worth having early: a way to reset, and something that maps the volume/back keys, or
the app becomes hard to leave.

### Step 6 — audio — done

`Audio` in `player.rs`: one cpal output stream reading a ring buffer that `App::step` fills
from `with_audio`, resampling the core's rate to the device's by linear interpolation (VICE
asks for 48 kHz here and the desktop device runs at 44.1). It is opened in `resumed()` and
dropped in `suspended()` with the surface, and a failure to open one only logs — the player
still runs silent.

cpal 0.17 reaches Android through AAudio (the `ndk` crate) rather than the oboe host earlier
versions used, so no C++ runtime has to be shipped beside the core. AAudio is API 26, and
`libaaudio.so` only exists in the 26+ sysroot, so `-P 26` and `minSdk = 26` — both were 24.

There is no drift control: the core is paced by the wall clock and the device by its own,
so the ring slowly fills or empties and glitches when it hits an end. demarc's own sink
(`src/audio.rs`) corrects that by nudging a rubato resample ratio from the ring's occupancy,
which is the thing to port here if it turns out to matter.

### Step 7 — rendering and performance

- Ask for `Backends::VULKAN | Backends::GL`. Vulkan is present on essentially every device
  that matters; GLES 3 is the fallback and is why the limits are `downlevel_defaults()`.
- Handle rotation: `WindowEvent::Resized` reconfigures, and `Outdated`/`Lost` already
  reconfigure in `Gfx::render`.
- **Cost.** lottes does ~40 texture fetches per output pixel. At 720x576 that is nothing;
  at 2400x1080 on a phone GPU it is not. If it costs too much, render the CRT pass into a
  fixed-size intermediate (say 1280x960) and blit that up, which is what demarc's
  composite pass does for the same reason.
- Do not step the core while suspended: `step()` currently runs the emulator even with no
  `Gfx`. Gate it, or the app burns battery in the background.

### Step 8 — content — media bundled

`android/app/src/main/assets/` is packaged into the APK and is where the media lives:

```
android/app/src/main/assets/tar_v2_pal.crt   Turbo Action ROM v2, a Retro Replay cartridge
android/app/src/main/assets/disk.d64
```

Both are checked in, and Gradle repackages the APK whenever either file's contents change.

A core cannot read out of the APK, so `android_main` copies each asset into the app's data
directory before loading the core (`ASSETS` in `player.rs`, overwriting every launch so a
rebuilt APK wins). VICE looks for cartridge images under `<system dir>/vice/<machine>/`,
which is why the `.crt` lands in `vice/C64/`; the `.d64` is handed to the core as the game.
`vice_cartridge` names the cartridge and `vice_autostart` is `disabled`, so the machine
comes up in the cartridge's own boot menu with the disk in drive 8.

VICE only attaches cartridges as `.crt`, and a flash dump like Turbo Action ROM is a bare
64 KiB of eight 8 KiB banks, so `scripts/rr_bin2crt.py` wraps one in the CRT container
(hardware type 36, Retro Replay) — the same shape as `system/vice/C64/rr38ppal.crt`:

```sh
scripts/rr_bin2crt.py tar_v2_pal.bin android/app/src/main/assets/tar_v2_pal.crt
```

What is still missing is the user's own files, which means the Storage Access Framework.
Downloading cores at runtime stays off the table — the W^X rule in step 2 is why RetroArch
ships its cores inside the APK.

## Order of work

1. ~~Workspace split (`retro-core`, `newsys`, `retroarc`), desktop build still green.~~ done
2. ~~`cargo ndk -t arm64-v8a check -p retro-core --features player` — compiling for the
   target.~~ done, and it links too
3. ~~Android logging + `internal_data_path` + load-in-place `dlopen` (step 2 above).~~ done
4. ~~`android_main`, Gradle project, first APK. Target: the BASIC banner on a device.~~ done
5. Touch input.
6. ~~Audio.~~ done
7. Performance pass.
8. ~~Bundled cartridge + disk as APK assets.~~ done; the user's own files are left.

## Toolchain on this machine

Almost everything was already installed; only the SDK command-line tools were missing and
have now been added.

| Tool | Version | Where |
|---|---|---|
| Android Studio | AI-253.29346.138.2531.14876573 | `/opt/android-studio` (bundled JDK 21 in `jbr/`) |
| SDK | platforms 36, 36.1 · build-tools 36.0.0, 36.1.0 · platform-tools · emulator | `~/Android/Sdk` (`$ANDROID_HOME`) |
| NDK | 29.0.14206865 (clang 21) | `~/Android/Sdk/ndk/29.0.14206865` |
| SDK command-line tools | 19.0 (`sdkmanager`, `avdmanager`, `apkanalyzer`) | `~/Android/Sdk/cmdline-tools/latest` — installed for this |
| cargo-ndk | 4.1.2 | `~/.cargo/bin` |
| Rust targets | `aarch64-linux-android`, `x86_64-linux-android` | rustup |
| Gradle | 9.7.1 system, JVM 26 | prefer the wrapper + `/opt/android-studio/jbr` (JDK 21); AGP does not follow JDK 26 |
| Emulator images | `android-36`, `android-36.1` google_apis_playstore | no AVD created yet, and no device is attached to `adb` |

Note the API-level flag is `-P`/`--platform` in cargo-ndk 4 (`-p` is cargo's `--package`).
