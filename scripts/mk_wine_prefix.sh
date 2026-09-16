#!/usr/bin/env bash

PREFIX="${1:-$HOME/.wine-demarc}"

export WINEPREFIX="$PREFIX"
export WINEARCH=win64

for tool in wine cabextract bwrap; do
    command -v "$tool" >/dev/null || {
        echo "error: $tool not found in PATH" >&2
        exit 1
    }
done

# Get latest winetricks
curl -fsSL -o winetricks https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks

# Set up wine prefix with all native D3D
wineboot -i
./winetricks -q dxvk d3dx9 d3dx10 vkd3d
./winetricks -q d3dx11_42 d3dx11_43 d3dcompiler_42 d3dcompiler_43 d3dcompiler_46 d3dcompiler_47
./winetricks -q corefonts

# Speech (for Zoom 3)
./winetricks --force -q speechsdk
# Don't use EGL (For 1995 / Kwelers)
wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f
# Make sure we have 1:1 logical/physical pixel mapping
wine reg add 'HKCU\Control Panel\Desktop' /v LogPixels /t REG_DWORD /d 96 /f

missing=()
for plugin in asfdemux avdec_wmav2; do
    gst-inspect-1.0 --exists "$plugin" || missing+=("$plugin")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo ""
    echo "**Warning: gstreamer plugins not installed: $missing"
    echo "Try something like:"
    wcho "sudo pacman -S gst-plugins-ugly"
    echo ""
fi

# For Panic Room / FLT (and pobably others)

missing=()
for f in files/gm.dls files/tssoft32.acm files/tsd32.dll files/dcomp.dll files/winmm.dll; do
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

# For DirectComposition demos (Razor 1911 and others). See tools/compshim.
install "files/dcomp.dll" $PREFIX/drive_c/windows/system32/dcomp.dll

# For Crinkler range imports. See tools/winmm.
install "files/winmm.dll" $PREFIX/drive_c/windows/syswow64/winmm.dll
