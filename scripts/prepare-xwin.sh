#!/usr/bin/env bash
# Patch the cargo-xwin Windows SDK cache so the vendored unrar C++ sources build.
#
# `unarc-rs` depends on `unrar`, whose `unrar_sys` crate compiles the original
# unrar C++ sources with cc-rs. Those sources assume a real MSVC-on-Windows
# toolchain, which breaks cross-compilation in two ways this script fixes:
#
#  1. Case-sensitive filesystem. xwin splats the Windows SDK into a
#     lowercase-normalized tree and only symlinks the mixed-case spellings the
#     SDK itself uses. unrar's os.hpp spells two of them differently:
#         vendor/unrar/os.hpp:52: fatal error: 'PowrProf.h' file not found
#     ...and its `#pragma comment(lib, ...)` directives name libs by yet another
#     casing.
#
#  2. Host-vs-target confusion in unrar_sys' build.rs: it branches on
#     `cfg!(windows)`, which is the *host* when cross-compiling, so it emits
#     `cargo:rustc-link-lib=pthread` for an MSVC target:
#         lld-link: error: could not open 'pthread.lib'
#     An empty `pthread.lib` satisfies the reference; the unrar sources use
#     Win32 threads on Windows, so nothing is actually missing.
#
# Idempotent. Re-run after the xwin cache is wiped or re-splatted. Invoked
# automatically by `just win`.
#
# Note: the third piece of the puzzle is not here but in the `win` recipe --
# clang-cl needs `-mssse3 -maes` because unrar guards its SSE/AES-NI intrinsics
# with `__attribute__((target(...)))` behind `#ifdef __GNUC__`, which clang-cl
# does not define.
set -euo pipefail

CACHE="${XWIN_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/cargo-xwin}/xwin"
[ -d "$CACHE" ] || { echo "xwin cache not found: $CACHE" >&2; exit 1; }

INCLUDE_DIR="sdk/include/um"
LIB_DIR="sdk/lib/um/x86_64"

# <dir relative to $CACHE> <spelling the unrar sources use>
NEEDED=(
  "$INCLUDE_DIR PowrProf.h"   # os.hpp #include
  "$INCLUDE_DIR Wbemidl.h"    # os.hpp #include (WMI)
  "$LIB_DIR     PowrProf.lib" # os.hpp #pragma comment(lib, ...)
  "$LIB_DIR     Shlwapi.lib"  # os.hpp #pragma comment(lib, ...)
  "$LIB_DIR     wbemuuid.lib" # os.hpp #pragma comment(lib, ...)
)

for entry in "${NEEDED[@]}"; do
  # shellcheck disable=SC2086 # deliberate word splitting of the two columns
  set -- $entry
  dir="$CACHE/$1" want="$2"
  [ -d "$dir" ] || { echo "skip: no $dir" >&2; continue; }
  [ -e "$dir/$want" ] && continue

  # Any existing spelling will do; prefer the all-lowercase one for stability.
  have=$(ls "$dir" | grep -ix -- "$want" | sort | head -n1 || true)
  if [ -z "$have" ]; then
    echo "warn: no case-insensitive match for $1/$want" >&2
    continue
  fi
  ln -s "$have" "$dir/$want"
  echo "linked $1/$want -> $have"
done

stub="$CACHE/$LIB_DIR/pthread.lib"
if [ ! -e "$stub" ]; then
  llvm-lib /llvmlibempty "/out:$stub"
  echo "created $LIB_DIR/pthread.lib (empty stub)"
fi
