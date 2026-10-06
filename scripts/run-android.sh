#!/usr/bin/env bash
# Build the `minimarc` player as an Android cdylib, package it into an APK with the
# VICE core, install and launch it. See docs/ANDROID.md.
#
#   scripts/run-android.sh            release build (the default: debug Bevy-less
#                                     wgpu is still slow, and the .so is huge)
#   scripts/run-android.sh --debug    debug build
#   scripts/run-android.sh --logcat   also stream the app's log until ctrl-c
set -euo pipefail

cd "$(dirname "$0")/.."

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
# AGP does not run on JDK 26, which is what this machine's `java` is.
if [[ -z "${JAVA_HOME:-}" && -d /opt/android-studio/jbr ]]; then
    export JAVA_HOME=/opt/android-studio/jbr
fi
ADB="$ANDROID_HOME/platform-tools/adb"

ABI=arm64-v8a
# 26 is where AAudio — cpal's Android backend — starts.
API=26
PKG=com.demarc.c64
ACTIVITY=android.app.NativeActivity
CORE=vice_x64sc
JNILIBS=android/app/src/main/jniLibs/$ABI

CARGO_PROFILE=(--release)
GRADLE_TASK=assembleRelease
APK=android/app/build/outputs/apk/release/app-release.apk
LOGCAT=0

for arg in "$@"; do
    case "$arg" in
        --debug)
            CARGO_PROFILE=()
            GRADLE_TASK=assembleDebug
            APK=android/app/build/outputs/apk/debug/app-debug.apk
            ;;
        --logcat) LOGCAT=1 ;;
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

echo "==> cargo ndk ($ABI, API $API)"
cargo ndk -t "$ABI" -P "$API" -o android/app/src/main/jniLibs \
    build -p retro-core --features player --lib "${CARGO_PROFILE[@]}"

echo "==> gradle $GRADLE_TASK"
(cd android && ./gradlew --console=plain "$GRADLE_TASK")

echo "==> install ($(du -h "$APK" | cut -f1))"
"$ADB" install -r "$APK"

echo "==> launch"
"$ADB" logcat -c || true
"$ADB" shell am start -n "$PKG/$ACTIVITY"

if [[ "$LOGCAT" == 1 ]]; then
    echo "==> logcat (ctrl-c to stop)"
    "$ADB" logcat demarc:V retro:V RustStdoutStderr:V "*:E"
else
    echo "==> done. logs:  adb logcat demarc:V '*:E'"
fi
