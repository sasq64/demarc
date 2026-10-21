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
chmod +x winetricks

# Set up wine prefix with all native D3D
wineboot -i
./winetricks -q dxvk d3dx9 d3dx10 vkd3d
./winetricks -q d3dx11_42 d3dx11_43 d3dcompiler_42 d3dcompiler_43 d3dcompiler_46 d3dcompiler_47
# winetricks' d3dcompiler_47 is 8.1 (6.3.9600); 10.0.15063 compiles typed UAV loads (One Of Those Days)
curl -fsSL https://github.com/lutris/d3d_extras/releases/download/v2/v2.tar.xz | tar -xJ -C "$PREFIX" v2/x32/d3dcompiler_47.dll v2/x64/d3dcompiler_47.dll
mv "$PREFIX/v2/x32/d3dcompiler_47.dll" "$PREFIX/drive_c/windows/syswow64/"
mv "$PREFIX/v2/x64/d3dcompiler_47.dll" "$PREFIX/drive_c/windows/system32/"
rm -r "$PREFIX/v2"
# d3dcsx_43 has no winetricks verb and no wine builtin (The Ink Test); take it from d3dx9's download
DXREDIST="${XDG_CACHE_HOME:-$HOME/.cache}/winetricks/directx9/directx_Jun2010_redist.exe"
cabextract -q -d "$PREFIX" -F 'Jun2010_d3dcsx_43_x*.cab' "$DXREDIST"
cabextract -q -d "$PREFIX/drive_c/windows/syswow64" -F d3dcsx_43.dll "$PREFIX/Jun2010_d3dcsx_43_x86.cab"
cabextract -q -d "$PREFIX/drive_c/windows/system32" -F d3dcsx_43.dll "$PREFIX/Jun2010_d3dcsx_43_x64.cab"
rm "$PREFIX"/Jun2010_d3dcsx_43_x*.cab
./winetricks -q corefonts
# MFC42 (Codename Chinadoll)
./winetricks -q mfc42
# Native msvcp60 from the same download; wine's lacks exports (Come Clean)
cabextract -q -d "$PREFIX/drive_c/windows/syswow64" -F msvcp60.dll "${XDG_CACHE_HOME:-$HOME/.cache}/winetricks/vcrun6/vcredist.exe"

# Speech (for Zoom 3)
./winetricks --force -q speechsdk
# Don't use EGL (For 1995 / Kwelers)
wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f
# Make sure we have 1:1 logical/physical pixel mapping
wine reg add 'HKCU\Control Panel\Desktop' /v LogPixels /t REG_DWORD /d 96 /f

# Keep wine off the game controllers and USB: on a Steam Deck it takes the built-in
# controller away from the kernel and Steam, and every button dies until a reboot
wine reg add 'HKCU\Software\Wine\DllOverrides' /v wineusb.sys /t REG_SZ /d '' /f
wine reg add 'HKLM\System\CurrentControlSet\Services\winebus' /v DisableHidraw /t REG_DWORD /d 1 /f
wine reg add 'HKLM\System\CurrentControlSet\Services\winebus' /v 'Enable SDL' /t REG_DWORD /d 0 /f
wine reg add 'HKLM\System\CurrentControlSet\Services\winebus' /v DisableInput /t REG_DWORD /d 1 /f

missing=()
for plugin in asfdemux avdec_wmav2; do
    gst-inspect-1.0 --exists "$plugin" || missing+=("$plugin")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo ""
    echo "**Warning: gstreamer plugins not installed: $missing"
    echo "Try something like:"
    echo "sudo pacman -S gst-plugins-ugly"
    echo ""
fi

"$(dirname "$0")/wine_prefix_files.sh" "$PREFIX"
