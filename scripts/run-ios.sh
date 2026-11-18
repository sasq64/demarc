#!/usr/bin/env bash
# Build `minimarc` for the iOS simulator, bundle it into an .app with the VICE
# core, the csdb database and the fast-load cartridge, install and launch it.
# Runs on a Mac with Xcode. See docs/iOS.md.
#
#   scripts/run-ios.sh              release build
#   scripts/run-ios.sh --debug      debug build
#   scripts/run-ios.sh --console    stay attached and print the app's log
#   scripts/run-ios.sh --device     build and bundle for a real device, and stop:
#                                   signing and installing it is not scripted
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=aarch64-apple-ios-sim
# Matches MinimumOSVersion in ios/Info.plist. Rust's default of 10.0 does not
# link the device build.
export IPHONEOS_DEPLOYMENT_TARGET=14.0
BUNDLE_ID=com.demarc.c64
CORE=vice_x64sc
SIM="${SIM:-iPhone 15}"
OUT=target/ios

PROFILE=release
CARGO_PROFILE=(--release)
CONSOLE=0

for arg in "$@"; do
    case "$arg" in
        --debug) PROFILE=debug; CARGO_PROFILE=() ;;
        --console) CONSOLE=1 ;;
        --device) TARGET=aarch64-apple-ios ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

OUT=$OUT/$TARGET
APP=$OUT/minimarc.app
mkdir -p "$OUT"

# Cores cannot be downloaded at runtime — iOS only loads code signed with the
# app — so the core is bundled. The buildbot has a device build only; the
# simulator takes it once its platform load command says "simulator".
if [[ ! -f "$OUT/${CORE}_libretro.dylib" ]]; then
    echo "==> fetching $CORE"
    url=https://buildbot.libretro.com/nightly/apple/ios-arm64/latest/${CORE}_libretro_ios.dylib.zip
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/core.zip" "$url"
    unzip -qo "$tmp/core.zip" -d "$tmp"
    if [[ "$TARGET" == *-sim ]]; then
        xcrun vtool -set-build-version iossim 14.0 18.0 -replace \
            -output "$OUT/${CORE}_libretro.dylib" "$tmp/${CORE}_libretro_ios.dylib"
    else
        mv "$tmp/${CORE}_libretro_ios.dylib" "$OUT/${CORE}_libretro.dylib"
    fi
    rm -rf "$tmp"
fi

echo "==> cargo build ($TARGET)"
cargo build -p minimarc --bin minimarc --target "$TARGET" "${CARGO_PROFILE[@]}"

echo "==> bundle"
rm -rf "$APP"
mkdir -p "$APP"
cp "target/$TARGET/$PROFILE/minimarc" ios/Info.plist "$APP/"
cp "$OUT/${CORE}_libretro.dylib" demodb/csdb.txt system/vice/C64/*.crt "$APP/"
# Only the csdb entries, each with the comment lines right above it.
awk '
    /^\[[a-z]+\.[0-9]+\]$/ { keep = /^\[csdb\./; if (keep) printf "%s", pend; pend = "" }
    keep { print; next }
    /^#/ { pend = pend $0 "\n"; next }
    { pend = "" }
' system/overrides.toml > "$APP/overrides.toml"
if [[ "$TARGET" != *-sim ]]; then
    echo "==> $APP is unsigned: sign the core and the app with a development"
    echo "    identity and a provisioning profile before installing it"
    exit 0
fi
codesign --force --sign - "$APP/${CORE}_libretro.dylib"
codesign --force --sign - "$APP"

echo "==> install on \"$SIM\" ($(du -sh "$APP" | cut -f1))"
xcrun simctl bootstatus "$SIM" -b >/dev/null
xcrun simctl terminate "$SIM" "$BUNDLE_ID" 2>/dev/null || true
xcrun simctl install "$SIM" "$APP"

echo "==> launch"
export SIMCTL_CHILD_RUST_LOG="${RUST_LOG:-info}"
if [[ "$CONSOLE" == 1 ]]; then
    xcrun simctl launch --console-pty "$SIM" "$BUNDLE_ID"
else
    xcrun simctl launch "$SIM" "$BUNDLE_ID"
    echo "==> done. screenshot:  xcrun simctl io \"$SIM\" screenshot shot.png"
fi
