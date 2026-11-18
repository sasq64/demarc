# iOS

`minimarc` (`crates/minimarc`) as an iOS app. It is the same binary target as
the desktop build — winit runs `main` on iOS — bundled by hand into an `.app`;
there is no Xcode project.

```sh
scripts/run-ios.sh --console     # on a Mac: build, bundle, install and launch in the simulator
scripts/run-ios.sh --device      # build and bundle for a phone; signing is not scripted
```

The script needs `rustup target add aarch64-apple-ios-sim aarch64-apple-ios`
and `demodb/csdb.txt`, which is not checked in.

## What differs from the desktop

- The bundle (`target/ios/<target>/minimarc.app`) holds the executable,
  `ios/Info.plist`, the core, `csdb.txt`, `rr38ppal-auto.crt` and the csdb part
  of `overrides.toml`, all flat beside each other.
- `player::ios_setup` points the system dir at `Library/Application Support`
  and the release cache at `Library/Caches/releases` in the app's container,
  and copies the cartridge and the overrides out of the bundle.
- The core is found beside the executable (`find_core` already looks there)
  and loaded in place, as on Android: iOS only loads code signed with the app.
- The buildbot only has a device build of the core
  (`nightly/apple/ios-arm64/latest`). For the simulator the script rewrites its
  platform load command with `vtool -set-build-version iossim`.
- The surface is the window's *outer* size; winit's inner size on iOS is the
  safe area, which goes to egui as `safe_area_insets` (`safe_area` in
  `player.rs`).
- RAR and LHA work, unlike on Android: `unarc-rs` builds, `release-io/build.rs`
  links `c++` for it.
- `UIApplication.idleTimerDisabled` keeps the screen on.

## Looking at the simulator

```sh
xcrun simctl io "iPhone 15" screenshot shot.png
xcrun simctl spawn "iPhone 15" log show --last 30s --predicate 'process == "minimarc"'
```

`simctl` cannot send touches, and without the Simulator app's window open
there is nothing to click on, so over ssh the app can only be looked at.

## State

Seen in the simulator (iPhone 15, iOS 17.4): the database loads, the picker
fills the screen inside the safe area, and a release downloads, unpacks and
runs in VICE with the CRT filter. That run was started by a temporary hook, not
by a touch.

Not tested: touch (taps, swipes, the key bar), sound, the soft keyboard (its
height is not fed to the picker as it is on Android), rotation, and anything
on a real device — the Mac had no signing identity. On a phone the swipe up for
the key bar will compete with the home gesture.
