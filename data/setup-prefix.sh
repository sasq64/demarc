#!/usr/bin/env bash
#
# Set up a wine prefix that can run "Panic Room" by Fairlight (Assembly 2008, PC 64k).
#
#   ./setup-prefix.sh [WINEPREFIX]        default: ~/.wine-panic
#
# Four things are needed. Each of the first three is a hard crash if missing; the
# fourth is a rendering fault. All four were diagnosed by debugging the actual
# faults, so the reasoning is recorded here rather than left as folklore.
#
#  1. native d3dx9_31
#       The intro compiles ~400 shaders through D3DX. Wine's builtin d3dx9 routes
#       them to vkd3d-shader, which returns NULL for at least one; the intro does
#       not check and dereferences it:
#           page fault on read access to 00000000 at 004073F1
#       Installing the file is not enough on its own -- the DLL override has to be
#       registered too, which is why this script sets it explicitly below.
#
#  2. gm.dls, the Microsoft GS Wavetable sample bank
#       Rather than spend 64k on samples, the intro borrows Windows' own bank. It
#       reads HKLM\Software\Microsoft\DirectMusic -> GMFilePath, fopen()s that path
#       and freads up to 4 MB of it, then indexes into the buffer by hardcoded byte
#       offsets. Wine creates the registry value but ships no gm.dls (it is
#       proprietary), so the fopen fails. The failure is unchecked -- the store to
#       the blob pointer is jumped over -- leaving it NULL, and every sample load
#       then reads its offset from address zero:
#           page fault on read access to 0012A36A at 0041003F
#       Because the offsets are file-specific, only the genuine Microsoft gm.dls
#       (3,440,660 bytes) decodes correctly. A substitute DLS avoids the crash but
#       produces noise.
#
#  3. the DSP Group TrueSpeech ACM codec
#       The rap vocal is TrueSpeech (WAVE_FORMAT_DSPGROUP_TRUESPEECH, tag 0x22).
#       Wine registers only imaadpcm, msadpcm, msg711, l3acm and msgsm610, so
#       acmStreamOpen fails with 512 (ACMERR_NOTPOSSIBLE). The intro ignores that
#       and calls acmStreamSize/PrepareHeader/Convert/Close on the NULL handle.
#       This is the "you might not get the rap in the music" case the release notes
#       warn about. Needs two files: tssoft32.acm (the ACM driver shim) and
#       tsd32.dll (the actual decoder it imports).
#
#  4. DXVK
#       Verified by testing: with wine's builtin d3d9 the intro runs to completion
#       but renders far too dark. DXVK renders it correctly.
#
# The three proprietary files cannot be downloaded by this script. Copy them from a
# Windows installation into the same directory as this script:
#
#   gm.dls        C:\Windows\System32\drivers\gm.dls
#   tssoft32.acm  C:\Windows\System32\tssoft32.acm
#   tsd32.dll     C:\Windows\System32\tsd32.dll
#
set -euo pipefail

PREFIX="${1:-$HOME/.wine-panic}"
ASSETS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

export WINEPREFIX="$PREFIX"
export WINEDEBUG="${WINEDEBUG:--all}"

# A win64 prefix is fine -- the intro is 32-bit and runs under WoW64.
export WINEARCH=win64

for tool in wine winetricks; do
    command -v "$tool" >/dev/null || { echo "error: $tool not found in PATH" >&2; exit 1; }
done

# Fail early and together, rather than halfway through the install.
missing=()
for f in gm.dls tssoft32.acm tsd32.dll; do
    [ -f "$ASSETS/$f" ] || missing+=("$f")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "error: missing file(s) in $ASSETS: ${missing[*]}" >&2
    echo "       copy them from a Windows install (see the header of this script)" >&2
    exit 1
fi

# gm.dls is indexed by hardcoded offsets, so warn loudly if it is not the stock one.
expected_size=3440660
actual_size=$(stat -c %s "$ASSETS/gm.dls")
if [ "$actual_size" != "$expected_size" ]; then
    echo "warning: gm.dls is $actual_size bytes, expected $expected_size." >&2
    echo "         If this is not the stock Microsoft file the music will be wrong." >&2
fi

echo "==> Creating prefix at $PREFIX"
wineboot -u

echo "==> Installing DXVK and native d3dx9_31"
winetricks -q dxvk d3dx9_31

# winetricks normally registers these overrides itself, but a prefix can end up with
# the native files present and no override recorded -- in which case wine silently
# keeps using its builtin and you get crash (1) with the DLLs apparently installed.
# Set them explicitly so the prefix cannot drift into that state.
echo "==> Registering DLL overrides"
for dll in d3dx9_31 d3d9; do
    wine reg add 'HKCU\Software\Wine\DllOverrides' /v "$dll" /t REG_SZ /d native /f
done

# A 32-bit process has C:\windows\system32 redirected to syswow64, so the codec and
# its decoder go there. gm.dls is written to both so the path resolves either way.
echo "==> Installing TrueSpeech codec"
install -Dm644 "$ASSETS/tssoft32.acm" "$PREFIX/drive_c/windows/syswow64/tssoft32.acm"
install -Dm644 "$ASSETS/tsd32.dll"    "$PREFIX/drive_c/windows/syswow64/tsd32.dll"

# msacm32 enumerates codecs from Drivers32, picking up values named "msacm.*".
wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' \
    /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' \
    /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f

echo "==> Installing gm.dls"
install -Dm644 "$ASSETS/gm.dls" "$PREFIX/drive_c/windows/syswow64/drivers/gm.dls"
install -Dm644 "$ASSETS/gm.dls" "$PREFIX/drive_c/windows/system32/drivers/gm.dls"

# Wine already sets GMFilePath, but a bare prefix is not guaranteed to, and the
# intro has no fallback if the value is absent.
wine reg add 'HKLM\Software\Microsoft\DirectMusic' \
    /v GMFilePath /t REG_SZ /d 'C:\windows\system32\drivers\gm.dls' /f

wineserver -w

cat <<EOF

Done. Run it with:

    WINEPREFIX=$PREFIX wine $ASSETS/flt_panic_room.exe

The override registered above makes d3dx9_31 native for the whole prefix, so no
WINEDLLOVERRIDES is needed on the command line. Pick a resolution in the startup
dialog and press Go; the precalc is long (mostly shader compilation).
EOF
