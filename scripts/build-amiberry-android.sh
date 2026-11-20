#!/usr/bin/env bash
# Cross-compile external/amiberry's libretro core for Android into
#   android/app/src/main/jniLibs/$ABI/libamiberry_libretro.so
# The Makefile builds in the source tree, so it builds in a copy under target/.
set -euo pipefail

cd "$(dirname "$0")/.."

export ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-$(ls -d "$ANDROID_HOME"/ndk/* | sort -V | tail -1)}"
ABI="${ABI:-arm64-v8a}"
# Bionic has iconv from 28.
API=28
case "$ABI" in
    arm64-v8a) TRIPLE=aarch64-linux-android; ARCH=aarch64 ;;
    x86_64) TRIPLE=x86_64-linux-android; ARCH=x86_64 ;;
    *) echo "unsupported ABI: $ABI" >&2; exit 2 ;;
esac
TC=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
SRC=external/amiberry
BUILD=target/amiberry-android/$ABI
OUT=android/app/src/main/jniLibs/$ABI/libamiberry_libretro.so

mkdir -p "$BUILD" "$(dirname "$OUT")"
rsync -a --exclude=.git --exclude='*.o' --exclude='*.so' --exclude='build*/' "$SRC/" "$BUILD/"
cp scripts/amiberry-android/android_stub.cpp "$BUILD/libretro/"
make -C "$BUILD/libretro" -j"$(nproc)" -f Makefile -f "$PWD/scripts/amiberry-android/android.mk" \
    ANDROID_STUB=android_stub.o \
    platform=unix ARCH=$ARCH \
    CC="$TC/$TRIPLE$API-clang" CXX="$TC/$TRIPLE$API-clang++" \
    ZLIB_CFLAGS= ZLIB_LIBS=-lz
"$TC/llvm-strip" -o "$OUT" "$BUILD/libretro/amiberry_libretro.so"
echo "==> $OUT"
