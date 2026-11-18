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

## Looking at it

```sh
xcrun simctl io "iPhone 15" screenshot shot.png
xcrun simctl spawn "iPhone 15" log show --last 30s --predicate 'process == "minimarc"'
```

`simctl` cannot send touches, and without the Simulator window open there is
nothing to click, so over ssh the simulator can only be looked at.

## State (2026-11-18)

Simulator (iPhone 15, iOS 17.4), merged with the key bar and phone picker
from `minimarc`: the database loads and the picker fills the screen inside the
safe area. Earlier a release was downloaded, unpacked and run in VICE there.

Not yet seen on a phone. Untested: touch (taps, swipes, the key bar), sound,
the soft keyboard (its height is not fed to the picker as `MiniActivity` does
on Android), rotation, and background audio. On a phone the swipe up for the
key bar will compete with the home gesture.
