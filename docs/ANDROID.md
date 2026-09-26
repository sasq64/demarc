# Android

How to get `src/bin/c64.rs` — the minimal one-window VICE player — running on a phone,
and what has to change first. Nothing here has been built for Android yet; this is the
plan, with the parts that were actually checked marked as checked.

## Starting position

`c64` is already shaped for it: winit + wgpu with no Bevy and no librashader, the window
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

## The structural blocker: one package builds all of its dependencies

Cargo compiles every non-optional dependency of a package when building *any* target in
it, whether that target uses them or not (verified with a scratch crate: a bin that never
mentions `libc` still caused `libc` to be compiled). So `cargo ndk build --bin c64` asks
the whole demarc dependency set — Bevy, librashader, `musix` (C++), `mlua`/Luau (C++),
resvg, symphonia, ureq, suppaftp — plus `build.rs`'s vendored C (cbmconvert, ADFlib,
xDMS) to cross-compile to `aarch64-linux-android`. Most of that has nothing to do with the
Android app, and any one of it failing blocks the build.

`cargo ndk -t arm64-v8a -P 24 check --bin c64` was run to see how far it gets. It fails,
on two things that have nothing to do with the player:

- **`unrar_sys` 0.5.8** (pulled in by `unarc-rs`, which `utils.rs` uses for archives):
  `vendor/unrar/ulinks.cpp:39: error: use of undeclared identifier 'lutimes'` — bionic has
  `utimes` but not `lutimes`. Not fixable from here; the dependency has to be out of the
  Android build. Note this is why `retro-core` should take only
  `utils::strip_verbatim_prefix` (the one thing `retro_emu` needs from that module) rather
  than `utils.rs` whole.
- **`android-activity` 0.6.1**: "Either game-activity or native-activity must be enabled as
  features" — expected, and fixed by giving winit its Android feature (step 3).

It died on those before reaching `musix`, Luau or librashader, so assume there is more
behind them.

### Step 1 — split into a workspace

The minimum split, which also gives Android the `cdylib` target it needs:

```
Cargo.toml                 [workspace] members = ["demarc", "crates/retro-core", "crates/c64"]
crates/retro-core/         lib: backend.rs, libretro.rs, pixels.rs, retro_emu/,
                           strip_verbatim_prefix out of utils.rs (and *only* that — the rest
                           of utils.rs brings unarc-rs, which does not build for Android),
                           and retro_log_shim.c in its build.rs
                           deps: anyhow, libloading, tempfile, tracing, libc (unix)
crates/c64/                lib + bin: the player.
                           [lib] crate-type = ["rlib", "cdylib"]   (cdylib is what the APK loads)
                           deps: retro-core, winit, wgpu, pollster, dirs
demarc/                    everything else, depending on retro-core
```

`src/bin/c64.rs` then drops its five `#[path]` includes and `use`s `retro_core::` instead;
the `#[path]` trick and the `retro_emu/mod.rs` layout it forced exist only because the two
binaries currently share one package, so both can go away.

This is the bulk of the work and it is mechanical: `demarc`'s modules keep their
`crate::backend` / `crate::retro_emu` paths only if the re-export is kept, otherwise it is
a find-and-replace to `retro_core::`.

A cheaper interim, if the split is not wanted yet: give `crates/c64` its own package and
`#[path]`-include the shared sources from `../../src/`. It builds, but every module they
both touch has to stay dependency-free by hand, so it only pays off as a spike.

### Step 2 — make the libretro layer Android-safe

Three concrete things in `retro_emu`:

1. **Core duping must be skippable.** `RetroCoreDirect::new` copies the `.so` into a
   private temp dir and `dlopen`s the copy, so two instances get independent globals. On
   Android, loading native code from the app's writable data directory is exactly what the
   platform blocks (W^X). The core has to be `dlopen`ed from the APK's native library
   directory instead, by bare soname. Add a "load in place" path (an argument, or
   `#[cfg(target_os = "android")]`) that skips the copy — with one view there is nothing to
   dupe anyway.
2. **`mod process` has to go on Android.** `RetroCoreProcess` re-executes the current
   executable and uses `memfd_create`; an APK has no standalone executable to re-exec.
   Gate it `#[cfg(all(unix, not(target_os = "android")))]`.
3. **Nothing may print to stdout.** Cores `printf` freely and demarc silences that with a
   `dup2` on fd 1; on Android stdout goes nowhere at all, so core logging has to reach
   logcat through `RETRO_ENVIRONMENT_GET_LOG_INTERFACE` (already implemented) with a
   tracing subscriber that writes to `__android_log_write` — `tracing-android`, or
   `android_logger` behind `tracing-log`. `tracing_subscriber::fmt()` in `main` becomes
   platform-specific.

### Step 3 — the entry point

`main()` becomes a `run()` that both entry points call:

```rust
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;
    let event_loop = EventLoop::builder().with_android_app(app.clone()).build().unwrap();
    run(event_loop, app.internal_data_path());
}
```

winit needs the `android-native-activity` feature (or `android-game-activity`, see
*Input*), which pulls `android-activity` in — for the Android target only:

```toml
[target.'cfg(target_os = "android")'.dependencies]
winit = { version = "0.30", features = ["android-native-activity"] }
android-activity = "0.6"
```
 `AndroidApp::internal_data_path()` is the
system/save directory to hand the core — `dirs::cache_dir()` is not meaningful here — and
`asset_manager()` is how any bundled `.d64`/`.prg` would be read.

### Step 4 — the APK

A small Gradle project, with the Rust `cdylib` and the core dropped into `jniLibs`:

```
android/
  settings.gradle.kts, build.gradle.kts
  app/build.gradle.kts
  app/src/main/AndroidManifest.xml
  app/src/main/jniLibs/arm64-v8a/libc64.so                 <- cargo ndk output
  app/src/main/jniLibs/arm64-v8a/libvice_x64sc.so          <- renamed buildbot core
```

With `android-native-activity` no Java is needed; the manifest points at
`android.app.NativeActivity` and names the library:

```xml
<activity android:name="android.app.NativeActivity"
          android:exported="true"
          android:configChanges="orientation|keyboardHidden|screenSize|density"
          android:screenOrientation="sensorLandscape">
  <meta-data android:name="android.app.lib_name" android:value="c64" />
  <intent-filter>
    <action android:name="android.intent.action.MAIN" />
    <category android:name="android.intent.category.LAUNCHER" />
  </intent-filter>
</activity>
```

Give the core the `lib` prefix (`libvice_x64sc.so`): Gradle packages it, the loader puts it
on the default search path, and `dlopen("libvice_x64sc.so")` by bare name then works with
`extractNativeLibs` either way. `find_core()` in `c64.rs` gets an Android arm that returns
that bare name instead of searching `$DEMARC_CORE_DIR` and the demarc cache.

Build and install:

```sh
cargo ndk -t arm64-v8a -P 24 -o android/app/src/main/jniLibs build --release
(cd android && ./gradlew installDebug)
adb logcat -s c64:V RustStdoutStderr:V
```

### Step 5 — input

Physical keys already work — a USB/Bluetooth keyboard arrives as ordinary winit
`KeyboardInput` and goes straight through the existing `retro_key` table. Touch does not,
and that is the real work:

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

### Step 6 — audio

The desktop binary drains the core's samples and drops them. On Android the natural path is
`cpal` 0.17's oboe host (`target_os = "android"` deps `oboe`, `ndk`, `ndk-context`, plus the
`oboe-shared-stdcxx` feature unless libc++ is linked statically) — which is also what the
desktop build would use, so one audio path serves both. `ndk_context` must be initialized
before the stream opens; `android-activity` does that as part of `android_main`. Expect to
resample: VICE asks for ~44.1 kHz, the device will want 48 kHz.

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

### Step 8 — what comes after it boots

Loading actual demos needs a content path: bundle a few `.d64`/`.prg` as assets first
(`AssetManager`), and only then consider the Storage Access Framework for the user's own
files. Downloading cores at runtime stays off the table — the W^X rule in step 2 is why
RetroArch ships its cores inside the APK.

## Order of work

1. Workspace split (`retro-core`, `c64`), desktop build still green.
2. `cargo ndk -t arm64-v8a check` on `crates/c64` — get it compiling for the target.
3. Android logging + `internal_data_path` + load-in-place `dlopen`, still on desktop.
4. `android_main`, Gradle project, first APK. Target: the BASIC banner on a device.
5. Touch input.
6. Audio.
7. Performance pass, content loading.

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
