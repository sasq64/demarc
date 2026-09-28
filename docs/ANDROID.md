# Android

How to get the minimal one-window VICE player — `crates/retro-core/src/player.rs`, run by
`src/bin/c64.rs` on the desktop — onto a phone, and what has to change first. The
workspace split and the cross-build are done; the app itself is not.

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
Cargo.toml           [workspace] members = ["crates/newsys", "crates/retro-core", "crates/retroarc"]
crates/retro-core/   lib: backend.rs, libretro.rs, pixels.rs, retro_emu/, find.rs, path.rs
                     (`strip_verbatim_prefix` — the one thing retro_emu wanted from utils.rs,
                     whose rest pulls unarc-rs), retro_log_shim.c in its build.rs, and the
                     player behind an optional `player` feature.
                     [lib] crate-type = ["rlib", "cdylib"]   (cdylib is what the APK loads)
                     deps: anyhow, libloading, tempfile, tracing, dirs, libc (unix)
crates/newsys/       everything between a file on disk and a Box<dyn Backend>
crates/retroarc/     the .slangp filter chains; the only crate that links librashader
src/                 demarc: the Bevy app
```

`src/bin/c64.rs` is now a thin `main` over `retro_core::player`; the `#[path]` includes and
the `retro_emu/mod.rs` layout they forced are gone.

**This works.** Both of these pass:

```sh
cargo ndk -t arm64-v8a -P 24 check -p retro-core                     # the core layer alone
cargo ndk -t arm64-v8a -P 24 check -p retro-core --features player --lib
```

and the library actually builds:

```sh
cargo ndk -t arm64-v8a -P 24 -o android/app/src/main/jniLibs \
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

winit's `android-native-activity` feature (or `android-game-activity`, see *Input*) is
already set for the Android target in `crates/retro-core/Cargo.toml`, which is what pulls
`android-activity` in. `android_main` itself still has to be written.
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
  app/src/main/jniLibs/arm64-v8a/libretro_core.so          <- cargo ndk output
  app/src/main/jniLibs/arm64-v8a/libvice_x64sc.so          <- renamed buildbot core
```

With `android-native-activity` no Java is needed; the manifest points at
`android.app.NativeActivity` and names the library:

```xml
<activity android:name="android.app.NativeActivity"
          android:exported="true"
          android:configChanges="orientation|keyboardHidden|screenSize|density"
          android:screenOrientation="sensorLandscape">
  <meta-data android:name="android.app.lib_name" android:value="retro_core" />
  <intent-filter>
    <action android:name="android.intent.action.MAIN" />
    <category android:name="android.intent.category.LAUNCHER" />
  </intent-filter>
</activity>
```

Give the core the `lib` prefix (`libvice_x64sc.so`): Gradle packages it, the loader puts it
on the default search path, and `dlopen("libvice_x64sc.so")` by bare name then works with
`extractNativeLibs` either way. `find_core()` — now `crates/retro-core/src/find.rs`, which
is where it was moved for exactly this — gets an Android arm that returns that bare name
instead of searching `$DEMARC_CORE_DIR` and the demarc cache.

Build and install:

```sh
cargo ndk -t arm64-v8a -P 24 -o android/app/src/main/jniLibs \
    build -p retro-core --features player --lib --release
(cd android && ./gradlew installDebug)
adb logcat -s retro_core:V RustStdoutStderr:V
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

1. ~~Workspace split (`retro-core`, `newsys`, `retroarc`), desktop build still green.~~ done
2. ~~`cargo ndk -t arm64-v8a check -p retro-core --features player` — compiling for the
   target.~~ done, and it links too
3. Android logging + `internal_data_path` + load-in-place `dlopen`, still on desktop
   (step 2 above).
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
