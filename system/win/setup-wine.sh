#!/usr/bin/env bash
# Sets up what demarc needs to run Windows demos: wine, bubblewrap, cabextract
# and the ~/.wine-demarc prefix with demarc's own DLLs (prefix/ next to this
# script). Every step runs every time, so running it again repairs whatever an
# earlier run left half done; downloads are kept and not fetched again.
# `demarc --setup-wine` runs the copy demarc carries.
#
#   WINEPREFIX  the prefix to set up (default ~/.wine-demarc)
set -Eeuo pipefail

WINE_VERSION=11.17
WINE_URL="https://github.com/Kron4ek/Wine-Builds/releases/download/$WINE_VERSION/wine-$WINE_VERSION-amd64-wow64.tar.xz"

export WINEPREFIX="${WINEPREFIX:-$HOME/.wine-demarc}"
export WINEARCH=win64
# demarc puts this ahead of PATH itself, so a wine unpacked here needs no setup
WINE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/demarc/wine"
export PATH="$WINE_DIR/bin:$PATH"
WINETRICKS_CACHE="${W_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/winetricks}"
DOWNLOADS="${XDG_CACHE_HOME:-$HOME/.cache}/demarc/setup-wine"

here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
files="$here/prefix"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
trap 'warn "Failed: $BASH_COMMAND (line $LINENO). Fix that and run again (a bad download: remove it from $DOWNLOADS)."' ERR

warn() { echo "** $*" >&2; }
die() {
    warn "$*"
    exit 1
}
has() { command -v "$1" >/dev/null; }

# fetch <url> <name>: download to $DOWNLOADS/<name> unless it is already there
fetch() {
    [ -s "$DOWNLOADS/$2" ] && return 0
    mkdir -p "$DOWNLOADS"
    curl -fL -o "$DOWNLOADS/$2.part" "$1"
    mv "$DOWNLOADS/$2.part" "$DOWNLOADS/$2"
}

detect_system() {
    local id="" like=""
    if [ -f /etc/os-release ]; then
        id=$(. /etc/os-release && echo "${ID:-}")
        like=$(. /etc/os-release && echo "${ID_LIKE:-}")
    fi
    case " $id $like " in
        *" steamos "*) echo steamdeck ;;
        *" arch "*) echo arch ;;
        *" debian "* | *" ubuntu "*) echo debian ;;
        *" fedora "* | *" rhel "*) echo fedora ;;
        *" suse "* | *" opensuse "*) echo suse ;;
        *) echo unknown ;;
    esac
}

install_packages() {
    case $SYSTEM in
        arch) sudo pacman -S --needed --noconfirm "$@" ;;
        debian) sudo apt-get update -q && sudo apt-get install -y "$@" ;;
        fedora) sudo dnf install -y "$@" ;;
        suse) sudo zypper install -y "$@" ;;
        *) return 1 ;;
    esac
}

# need_tool <command> <package>
need_tool() {
    has "$1" && return 0
    echo "Installing $2"
    install_packages "$2" || die "Could not install $2, install it yourself"
    has "$1" || die "$1 still not found after installing $2"
}

# A system wine is used as is; one this script unpacked is replaced unless it
# is complete and the right version.
install_wine() {
    if [ -x "$WINE_DIR/bin/wine" ] && [ "$(cat "$WINE_DIR/.version" 2>/dev/null)" = "$WINE_VERSION" ]; then
        return 0
    fi
    if [ ! -e "$WINE_DIR" ] && has wine; then
        return 0
    fi
    [ "$(uname -m)" = x86_64 ] || die "No wine found, and only x86_64 builds are downloaded"
    echo "Installing wine $WINE_VERSION to $WINE_DIR"
    fetch "$WINE_URL" "wine-$WINE_VERSION-amd64-wow64.tar.xz"
    mkdir -p "$tmp/wine"
    tar -C "$tmp/wine" -xJf "$DOWNLOADS/wine-$WINE_VERSION-amd64-wow64.tar.xz" --strip-components=1
    echo "$WINE_VERSION" >"$tmp/wine/.version"
    rm -rf "$WINE_DIR"
    mkdir -p "$(dirname "$WINE_DIR")"
    mv "$tmp/wine" "$WINE_DIR"
}

# SteamOS's gstreamer has neither the ASF demuxer nor libav, which WMA soundtracks need
install_steamdeck_gstreamer() {
    local gst="${XDG_DATA_HOME:-$HOME/.local/share}/demarc/gst" v pkg
    v=$(pacman -Q gstreamer | cut -d' ' -f2)
    if [ -f "$gst/libgstasf.so" ] && [ -f "$gst/libgstlibav.so" ] && [ "$(cat "$gst/.version" 2>/dev/null)" = "$v" ]; then
        return 0
    fi
    echo "Installing gstreamer plugins"
    mkdir -p "$tmp/gst"
    for pkg in gst-plugins-ugly:libgstasf.so gst-libav:libgstlibav.so; do
        local name="${pkg%:*}-$v-x86_64.pkg.tar.zst"
        fetch "https://archive.archlinux.org/packages/g/${pkg%:*}/$name" "$name"
        tar -C "$tmp/gst" --zstd -xf "$DOWNLOADS/$name" --strip-components=3 "usr/lib/gstreamer-1.0/${pkg#*:}"
    done
    echo "$v" >"$tmp/gst/.version"
    rm -rf "$gst"
    mv "$tmp/gst" "$gst"
}

check_gstreamer() {
    local missing=() plugin
    for plugin in asfdemux avdec_wmav2; do
        gst-inspect-1.0 --exists "$plugin" 2>/dev/null || missing+=("$plugin")
    done
    [ ${#missing[@]} -eq 0 ] && return 0
    warn "gstreamer plugins not installed: ${missing[*]} (demos with WMA music will be silent)"
    case $SYSTEM in
        arch) warn "Try: sudo pacman -S gst-plugins-ugly gst-libav" ;;
        debian) warn "Try: sudo apt-get install gstreamer1.0-plugins-ugly gstreamer1.0-libav" ;;
    esac
}

setup_prefix() {
    fetch https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks winetricks
    winetricks() { bash "$DOWNLOADS/winetricks" -q "$@"; }
    # winetricks skips the verbs this log lists; without it every verb is
    # installed again, from winetricks' own download cache
    rm -f "$WINEPREFIX/winetricks.log"
    local sys32="$WINEPREFIX/drive_c/windows/system32"
    local wow64="$WINEPREFIX/drive_c/windows/syswow64"

    wineboot -u

    # All native D3D
    winetricks dxvk d3dx9 d3dx10 vkd3d
    winetricks d3dx11_42 d3dx11_43 d3dcompiler_42 d3dcompiler_43 d3dcompiler_46 d3dcompiler_47
    # winetricks' d3dcompiler_47 is 8.1 (6.3.9600); 10.0.15063 compiles typed UAV loads (One Of Those Days)
    fetch https://github.com/lutris/d3d_extras/releases/download/v2/v2.tar.xz d3d_extras-v2.tar.xz
    tar -xJf "$DOWNLOADS/d3d_extras-v2.tar.xz" -C "$tmp" v2/x32/d3dcompiler_47.dll v2/x64/d3dcompiler_47.dll
    cp "$tmp/v2/x32/d3dcompiler_47.dll" "$wow64/"
    cp "$tmp/v2/x64/d3dcompiler_47.dll" "$sys32/"
    # d3dcsx_43 has no winetricks verb and no wine builtin (The Ink Test); take it from d3dx9's download
    cabextract -q -d "$tmp" -F 'Jun2010_d3dcsx_43_x*.cab' "$WINETRICKS_CACHE/directx9/directx_Jun2010_redist.exe"
    cabextract -q -d "$wow64" -F d3dcsx_43.dll "$tmp/Jun2010_d3dcsx_43_x86.cab"
    cabextract -q -d "$sys32" -F d3dcsx_43.dll "$tmp/Jun2010_d3dcsx_43_x64.cab"

    winetricks corefonts
    # Selawik, Microsoft's open metric-compatible Segoe UI stand-in (Epsilon by Mercury)
    fetch https://github.com/microsoft/Selawik/releases/download/1.01/Selawik_Release.zip Selawik-1.01.zip
    unzip -o -q -j "$DOWNLOADS/Selawik-1.01.zip" '*.ttf' -d "$WINEPREFIX/drive_c/windows/Fonts"
    wine reg add 'HKCU\Software\Wine\Fonts\Replacements' /v 'Segoe UI' /t REG_SZ /d Selawik /f

    # MFC42 (Codename Chinadoll)
    winetricks mfc42
    # Native msvcp60 from the same download; wine's lacks exports (Come Clean)
    cabextract -q -d "$wow64" -F msvcp60.dll "$WINETRICKS_CACHE/vcrun6/vcredist.exe"
    # Speech (for Zoom 3)
    winetricks --force speechsdk

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
}

# put <file> <dir under drive_c/windows>
put() {
    install -Dm644 "$files/$1" "$WINEPREFIX/drive_c/windows/$2/$1"
}

install_prefix_files() {
    [ -d "$files" ] || die "$files is missing: run demarc --setup-wine, or this script from demarc's system/win"

    # For Panic Room / FLT (and probably others)
    put tssoft32.acm syswow64
    put tsd32.dll syswow64
    wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
    wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
    put gm.dls syswow64/drivers
    put gm.dls system32/drivers
    wine reg add 'HKLM\Software\Microsoft\DirectMusic' /v GMFilePath /t REG_SZ /d 'C:\windows\system32\drivers\gm.dls' /f

    # Texas by Keyboarders changes to Vista's sample music folder and quits without it.
    mkdir -p "$WINEPREFIX/drive_c/users/Public/Music/Sample Music"

    # For DirectComposition demos (Razor 1911 and others). See tools/compshim.
    put dcomp.dll system32
    # For Crinkler range imports. See tools/winmm.
    put winmm.dll syswow64
    # For Crinkler imports of forwarded exports. See tools/kernelbase.
    put kernelbase.dll syswow64
    # For intros that take D3DX math from dx8vb. See tools/dx8vb.
    put dx8vb.dll syswow64
    # For intros that take GL 1.1 functions from wglGetProcAddress. See tools/opengl32.
    put opengl32.dll syswow64
}

SYSTEM=$(detect_system)
echo "System: $SYSTEM"

install_wine
if [ "$SYSTEM" = steamdeck ]; then
    # The root filesystem is read-only: the Deck bundle puts a cabextract on PATH
    if has cabextract && [ ! -x "$WINE_DIR/bin/cabextract" ]; then
        install -D "$(command -v cabextract)" "$WINE_DIR/bin/cabextract"
    fi
    install_steamdeck_gstreamer
    # Run over ssh: wine's windows go to the Deck's screen, and no mono/gecko prompts
    export DISPLAY="${DISPLAY:-:0}"
    export WINEDLLOVERRIDES="mscoree=d;mshtml=d;wineusb.sys=d"
    echo "Reboot the Deck afterwards if its buttons stop responding."
fi
need_tool wine wine
need_tool cabextract cabextract
need_tool unzip unzip
need_tool bwrap bubblewrap

echo "Setting up the wine prefix $WINEPREFIX with $(command -v wine) ($(wine --version))"
setup_prefix
install_prefix_files
# The registry only reaches disk when the wineserver exits
wineserver -w
if [ "$SYSTEM" != steamdeck ]; then
    check_gstreamer
fi
echo "Done."
