#!/usr/bin/env bash
# Installs the DLLs and data from files/ into an existing wine prefix. Run from
# the directory holding files/; mk_wine_prefix.sh ends with it.

PREFIX="${1:-$HOME/.wine-demarc}"
export WINEPREFIX="$PREFIX"

# For Panic Room / FLT (and pobably others)

missing=()
for f in files/gm.dls files/tssoft32.acm files/tsd32.dll files/dcomp.dll files/winmm.dll files/kernelbase.dll files/dx8vb.dll files/opengl32.dll; do
    [ -f "$f" ] || missing+=("$f")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo ""
    echo "**Warning: missing file(s): ${missing[*]}" >&2
    echo "This is not critical, but some demos will not work"
    exit 1
fi

install -Dm644 files/tssoft32.acm $PREFIX/drive_c/windows/syswow64/tssoft32.acm
install -Dm644 files/tsd32.dll $PREFIX/drive_c/windows/syswow64/tsd32.dll
wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
install -Dm644 files/gm.dls $PREFIX/drive_c/windows/syswow64/drivers/gm.dls
install -Dm644 files/gm.dls $PREFIX/drive_c/windows/system32/drivers/gm.dls
wine reg add 'HKLM\Software\Microsoft\DirectMusic' /v GMFilePath /t REG_SZ /d 'C:\windows\system32\drivers\gm.dls' /f

# Texas by Keyboarders changes to Vista's sample music folder and quits without it.
mkdir -p "$PREFIX/drive_c/users/Public/Music/Sample Music"

# For DirectComposition demos (Razor 1911 and others). See tools/compshim.
install "files/dcomp.dll" $PREFIX/drive_c/windows/system32/dcomp.dll

# For Crinkler range imports. See tools/winmm.
install "files/winmm.dll" $PREFIX/drive_c/windows/syswow64/winmm.dll
# For Crinkler imports of forwarded exports. See tools/kernelbase.
install "files/kernelbase.dll" $PREFIX/drive_c/windows/syswow64/kernelbase.dll
# For intros that take D3DX math from dx8vb. See tools/dx8vb.
install "files/dx8vb.dll" $PREFIX/drive_c/windows/syswow64/dx8vb.dll
# For intros that take GL 1.1 functions from wglGetProcAddress. See tools/opengl32.
install "files/opengl32.dll" $PREFIX/drive_c/windows/syswow64/opengl32.dll
