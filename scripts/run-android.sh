#!/usr/bin/env bash
# Build `minimarc` as an Android cdylib, package it into an APK with the VICE
# core, the csdb database and the fast-load cartridge, install and launch it.
# See docs/ANDROID.md.
#
#   scripts/run-android.sh            release build (the default: debug Bevy-less
#                                     wgpu is still slow, and the .so is huge)
#   scripts/run-android.sh --debug    debug build
#   scripts/run-android.sh --logcat   also stream the app's log until ctrl-c
#   scripts/run-android.sh --bundle   build the Play Store .aab instead; no install
#                                     (VERSION_CODE=N sets its versionCode)
set -euo pipefail

cd "$(dirname "$0")/.."

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
# AGP does not run on JDK 26, which is what this machine's `java` is.
if [[ -z "${JAVA_HOME:-}" ]]; then
    for jbr in /opt/android-studio/jbr "$HOME/sdk/android-studio/jbr"; do
        if [[ -d "$jbr" ]]; then export JAVA_HOME=$jbr; break; fi
    done
fi
ADB="$ANDROID_HOME/platform-tools/adb"

# ABI=x86_64 for the emulator.
ABI="${ABI:-arm64-v8a}"
# 26 is where AAudio — cpal's Android backend — starts.
API=26
PKG=se.minnberg.demarc
ACTIVITY=com.demarc.c64.MiniActivity
CORE=vice_x64sc
JNILIBS=android/app/src/main/jniLibs/$ABI
ASSETS=android/app/src/main/assets

CARGO_PROFILE=(--release)
GRADLE_TASK=assembleRelease
APK=android/app/build/outputs/apk/release/app-release.apk
LOGCAT=0
BUNDLE=0

for arg in "$@"; do
    case "$arg" in
        --debug)
            CARGO_PROFILE=()
            GRADLE_TASK=assembleDebug
            APK=android/app/build/outputs/apk/debug/app-debug.apk
            ;;
        --logcat) LOGCAT=1 ;;
        --bundle) BUNDLE=1; GRADLE_TASK=bundleRelease ;;
        *) echo "unknown option: $arg" >&2; exit 2 ;;
    esac
done

# Cores cannot be downloaded at runtime — Android will not load code out of the
# app's writable data dir — so the core is packaged beside our own library.
if [[ ! -f "$JNILIBS/lib${CORE}_libretro.so" ]]; then
    echo "==> fetching $CORE for $ABI"
    url=https://buildbot.libretro.com/nightly/android/latest/$ABI/${CORE}_libretro_android.so.zip
    tmp=$(mktemp -d)
    curl -sSfL -o "$tmp/core.zip" "$url"
    unzip -qo "$tmp/core.zip" -d "$tmp"
    mkdir -p "$JNILIBS"
    mv "$tmp/${CORE}_libretro_android.so" "$JNILIBS/lib${CORE}_libretro.so"
    rm -rf "$tmp"
fi

mkdir -p "$ASSETS"
rm -f "$ASSETS/csdb.txt.gz"
cp -u demodb/csdb.txt system/vice/C64/*.crt system/web/*.css "$ASSETS/"
# Only the csdb entries, each with the comment lines right above it.
awk '
    /^\[[a-z]+\.[0-9]+\]$/ { keep = /^\[csdb\./; if (keep) printf "%s", pend; pend = "" }
    keep { print; next }
    /^#/ { pend = pend $0 "\n"; next }
    { pend = "" }
' system/overrides.toml > "$ASSETS/overrides.toml"

echo "==> cargo ndk ($ABI, API $API)"
cargo ndk -t "$ABI" -P "$API" -o android/app/src/main/jniLibs \
    build -p minimarc --lib "${CARGO_PROFILE[@]}"
# sevenz-rust2 also builds as a cdylib, which cargo-ndk copies along.
rm -f "$JNILIBS"/libsevenz_rust2-*.so "$JNILIBS/libretro_core.so"

echo "==> gradle $GRADLE_TASK"
(cd android && ./gradlew --console=plain -Pabi="$ABI" -PversionCode="${VERSION_CODE:-1}" "$GRADLE_TASK")

if [[ "$BUNDLE" == 1 ]]; then
    echo "==> android/app/build/outputs/bundle/release/app-release.aab"
    exit 0
fi

echo "==> install ($(du -h "$APK" | cut -f1))"
# A Play install is signed with a different key and cannot be updated over.
if ! "$ADB" install -r "$APK"; then
    echo "==> uninstalling the existing $PKG and retrying"
    "$ADB" uninstall "$PKG" || true
    "$ADB" install "$APK"
fi

echo "==> launch"
"$ADB" logcat -c || true
"$ADB" shell am start -n "$PKG/$ACTIVITY"

if [[ "$LOGCAT" == 1 ]]; then
    echo "==> logcat (ctrl-c to stop)"
    "$ADB" logcat demarc:V retro:V RustStdoutStderr:V "*:E"
else
    echo "==> done. logs:  adb logcat demarc:V '*:E'"
fi
