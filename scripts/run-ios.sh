#!/usr/bin/env bash
# Build `minimarc` for the iOS simulator or a phone, bundle it into an .app with
# the VICE core, the csdb database and the carts, install and launch it.
# Runs on a Mac with Xcode. See docs/iOS.md.
#
#   scripts/run-ios.sh              release build in the simulator
#   scripts/run-ios.sh --debug      debug build
#   scripts/run-ios.sh --console    stay attached and print the app's log
#   scripts/run-ios.sh --device     sign with $TEAM and run on the connected phone
set -euo pipefail

cd "$(dirname "$0")/.."

TARGET=aarch64-apple-ios-sim
# Matches MinimumOSVersion in ios/Info.plist. Rust's default of 10.0 does not
# link the device build.
export IPHONEOS_DEPLOYMENT_TARGET=14.0
BUNDLE_ID="${BUNDLE_ID:-se.minnberg.demarc.dev}"
# The personal team of the Apple ID signed in to Xcode.
TEAM="${TEAM:-DGBP99FB75}"
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
/usr/libexec/PlistBuddy -c "Set :CFBundleIdentifier $BUNDLE_ID" "$APP/Info.plist"
cp "$OUT/${CORE}_libretro.dylib" demodb/csdb.txt system/vice/C64/*.crt "$APP/"
# Only the csdb entries, each with the comment lines right above it.
awk '
    /^\[[a-z]+\.[0-9]+\]$/ { keep = /^\[csdb\./; if (keep) printf "%s", pend; pend = "" }
    keep { print; next }
    /^#/ { pend = pend $0 "\n"; next }
    { pend = "" }
' system/overrides.toml > "$APP/overrides.toml"
if [[ "$TARGET" != *-sim ]]; then
    # Only Xcode makes provisioning profiles: build a stub app with the same
    # bundle id and take its profile and signing identity.
    echo "==> provisioning $BUNDLE_ID (team $TEAM)"
    prov=$PWD/$OUT/provision
    xcodebuild -quiet -project ios/provision/Provision.xcodeproj -sdk iphoneos \
        DEVELOPMENT_TEAM="$TEAM" BUNDLE_ID="$BUNDLE_ID" SYMROOT="$prov" \
        -allowProvisioningUpdates -allowProvisioningDeviceRegistration build
    stub=$prov/Debug-iphoneos/minimarc.app
    identity=$(codesign -dvv "$stub" 2>&1 | sed -n 's/^Authority=//p' | head -1)
    cp "$stub/embedded.mobileprovision" "$APP/"
    security cms -D -i "$APP/embedded.mobileprovision" > "$prov/profile.plist"
    /usr/libexec/PlistBuddy -x -c "Print :Entitlements" "$prov/profile.plist" \
        > "$prov/entitlements.plist"
    codesign --force --sign "$identity" "$APP/${CORE}_libretro.dylib"
    codesign --force --sign "$identity" --entitlements "$prov/entitlements.plist" "$APP"

    if [[ -z "${DEVICE:-}" ]]; then
        xcrun devicectl list devices --json-output "$prov/devices.json" >/dev/null
        DEVICE=$(python3 -c 'import json, sys
for d in json.load(open(sys.argv[1]))["result"]["devices"]:
    if d["hardwareProperties"].get("platform") == "iOS" and d["connectionProperties"].get("pairingState") == "paired":
        print(d["identifier"]); break' "$prov/devices.json")
        [[ -n "$DEVICE" ]] || { echo "no paired iPhone found" >&2; exit 1; }
    fi
    echo "==> install on $DEVICE ($(du -sh "$APP" | cut -f1))"
    xcrun devicectl device install app --device "$DEVICE" "$APP" >/dev/null
    echo "==> launch"
    launch=(--terminate-existing)
    [[ "$CONSOLE" == 1 ]] && launch+=(--console)
    xcrun devicectl device process launch "${launch[@]}" --device "$DEVICE" \
        --environment-variables "{\"RUST_LOG\": \"${RUST_LOG:-info}\"}" "$BUNDLE_ID"
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
