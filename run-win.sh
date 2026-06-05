#!/usr/bin/env bash
# Run the cross-compiled Windows build natively on Linux via wine, using the
# native RADV Vulkan driver on the host GPU (no VM, no DX12 translation).
#
#   wgpu Windows Vulkan backend -> winevulkan -> RADV -> Radeon 890M.
#
# All arguments are forwarded to demarc. Any argument that exists as a file or
# directory is converted from a Unix path to a Windows path (via `winepath -w`)
# so the Windows-side code receives an unambiguous, drive-qualified path
# (e.g. ~/Demo/Amiga -> Z:\home\sasq\Demo\Amiga). Flags and flag values that are
# not existing paths are passed through unchanged.
#
# Usage:
#   ./run-win.sh ~/Demo/Amiga
#   ./run-win.sh ~/Demo/Amiga --grid=5x4 --window
#   PROFILE=debug ./run-win.sh ~/Demo/Amiga
#
# One-time prefix setup this relies on (already applied once):
#   wine reg add "HKCU\Environment" /v XDG_CACHE_HOME /t REG_SZ \
#        /d "C:\users\sasq\.cache" /f
set -euo pipefail

cd "$(dirname "$0")"

PROFILE="${PROFILE:-release}"   # PROFILE=debug ./run-win.sh ... for the debug build
EXE="target/x86_64-pc-windows-msvc/${PROFILE}/demarc.exe"
[ -f "$EXE" ] || { echo "Build not found: $EXE" >&2; exit 1; }

# Convert existing-path arguments to Windows paths; pass everything else through.
args=()
for a in "$@"; do
  if [ -e "$a" ]; then
    win=$(WINEDEBUG=-all winepath -w "$(realpath "$a")" 2>/dev/null) \
      && args+=( "$win" ) || args+=( "$a" )
  else
    args+=( "$a" )
  fi
done

WGPU_BACKEND="${WGPU_BACKEND:-vulkan}" \
RUST_LOG="${RUST_LOG:-info}" \
RUST_BACKTRACE="${RUST_BACKTRACE:-1}" \
WINEDEBUG="${WINEDEBUG:--all}" \
  exec wine "$EXE" "${args[@]}"
