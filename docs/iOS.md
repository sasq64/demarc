# iOS

`minimarc` (`crates/minimarc`) as an iOS app: the same picker, navigator and
VICE player as on Android. It is the desktop binary target — winit runs `main`
on iOS — bundled by hand into an `.app`; the only Xcode project is a stub used
for provisioning.

```sh
scripts/run-ios.sh                      # build, bundle, install and launch in the simulator
scripts/run-ios.sh --device             # the same on the connected iPhone
scripts/run-ios.sh --device --console   # and stay attached to its output
```

Everything runs on a Mac (`ssh macbook`, checkout in `~/projects/demarc`).

## Requirements

- `rustup target add aarch64-apple-ios aarch64-apple-ios-sim`
- `demodb/csdb.txt`, which is not checked in.
- An Xcode that knows the phone's iOS version: a phone on iOS 26 needs
  Xcode 26. With an older one `devicectl` lists the phone but reports
  `ddiServicesAvailable: false` and cannot install to it.
- On the phone: trust the Mac, and Settings → Privacy & Security →
  Developer Mode on.
- The Apple ID signed in under Xcode → Settings → Accounts. When its login
  expires `xcodebuild` fails with "The login details for account … were
  rejected"; sign in there again.
- The first install from a free team has to be trusted on the phone:
  Settings → General → VPN & Device Management → Developer App → Trust.
- The phone unlocked: `devicectl` mounts the developer disk image on first use
  after a reboot and fails with `kAMDMobileImageMounterDeviceLocked` otherwise.
  Xcode 16.4 (the newest the App Store offers on Sequoia) works with iOS 26.6.
- Over ssh the login keychain is locked, and signing fails with "User
  interaction is not allowed" or `errSecInternalComponent`. Run the script in
  Terminal on the Mac, or `ssh -t macbook security unlock-keychain
  ~/Library/Keychains/login.keychain-db` first; it stays locked after a reboot.
  Launching an installed build (below) needs no keychain.

## Identity

| | |
|---|---|
| Bundle id | `se.minnberg.demarc.dev` (`BUNDLE_ID`), set into `ios/Info.plist`'s copy by the script |
| Team | `DGBP99FB75`, the personal (free) team (`TEAM`) |
| Executable | `minimarc` + `vice_x64sc_libretro.dylib` beside it |
| Minimum iOS | 14.0 (`MinimumOSVersion`, `IPHONEOS_DEPLOYMENT_TARGET`) |

The `.dev` id keeps `se.minnberg.demarc` (the Play package) free for a paid
team. A free team's profile lasts 7 days; after that the app no longer starts
and `--device` has to be run again.

## Signing

Only Xcode makes provisioning profiles, so `ios/provision/` holds a stub
project (an empty Swift `main`) with `PRODUCT_BUNDLE_IDENTIFIER` and
`DEVELOPMENT_TEAM` taken from the command line. `--device` builds it with
`-allowProvisioningUpdates -allowProvisioningDeviceRegistration`, which creates
the development certificate, registers the phone and fetches a profile, then:

- copies the stub's `embedded.mobileprovision` into the app,
- signs the core and then the app with the identity that signed the stub, using
  the entitlements in the profile,
- installs with `xcrun devicectl device install app` and launches with
  `devicectl device process launch` (`DEVICE` picks the phone; otherwise the
  first paired iPhone).

## What differs from the desktop

- The bundle (`target/ios/<target>/minimarc.app`) holds the executable,
  `Info.plist`, the core, `csdb.txt`, the carts from `system/vice/C64/` and
  the csdb part of `overrides.toml`, all flat beside each other.
- `player::ios_setup` points the system dir at `Library/Application Support`
  and the release cache at `Library/Caches/releases` in the app's container,
  and copies the carts and overrides out of the bundle (`ASSETS`, shared with
  Android). The database is read in place.
- The core is found beside the executable (`find_core` looks there) and
  loaded in place, as on Android: iOS only loads code signed with the app, so
  cores cannot be downloaded at runtime.
- The buildbot only has a device build of the core
  (`nightly/apple/ios-arm64/latest`). For the simulator the script rewrites its
  platform load command with `vtool -set-build-version iossim`.
- The surface is the window's *outer* size; winit's inner size on iOS is the
  safe area, which becomes egui's `safe_area_insets` (`Gfx::safe_area`).
- RAR and LHA work: `release-io/build.rs` links `c++` for `unrar_sys`.
- `UIApplication.idleTimerDisabled` keeps the screen on.
- The audio session is set to `Playback` (`play_audio`): the default one is
  muted by the silent switch. With `UIBackgroundModes` = `audio` in
  `Info.plist` the demo keeps playing in the background.
- The soft keyboard sends each character as a `KeyboardInput` with an
  `Unidentified` physical key and the character as the logical key, press and
  release together; Backspace is the only one with a key code. `window_event`
  sends those characters through `char_key`, with `\n` as Return.

## Looking at it

On the phone, with the log on the terminal (no keychain needed):

```sh
xcrun devicectl device process launch --terminate-existing --console \
    --environment-variables '{"RUST_LOG": "info", "MINIMARC_PERF": "1"}' \
    --device <id> se.minnberg.demarc.dev
```

`<id>` is from `xcrun devicectl list devices`. `MINIMARC_PERF` logs ticks,
core and draw rates and times every five seconds (`Perf` in `player.rs`).

In the simulator:

```sh
xcrun simctl io "iPhone 15" screenshot shot.png
xcrun simctl spawn "iPhone 15" log show --last 30s --predicate 'process == "minimarc"'
```

`simctl` cannot send touches, and without the Simulator window open there is
nothing to click, so over ssh the simulator can only be looked at.

## State (2026-11-18)

Running on an iPhone 13 (iOS 26.6.2) with the free team, from Xcode 16.4 on
macOS 15.7.3: the picker, download and unpack, VICE, sound, touch, the key bar
and the soft keyboard work.

Measured with `MINIMARC_PERF` during a demo: a steady 50.1 core frames and
draws per second, VICE 6.5 ms a frame (worst 9 ms), egui and render under
0.5 ms on the CPU, no tick late by more than 21 ms. Motion still looks uneven
because the panel runs at 60 Hz: every fifth refresh repeats a frame. Blending
the two nearest C64 frames per refresh would smooth it; not done.

Untested: rotation, the soft keyboard's height (not fed to the picker as
`MiniActivity` does on Android), background audio with the screen off. On a
phone the swipe up for the key bar competes with the home gesture.
