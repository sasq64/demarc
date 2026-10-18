#!/usr/bin/env bash
# Sets up what demarc needs to run Windows demos: wine, bubblewrap, cabextract
# and the ~/.wine-demarc prefix with demarc's own DLLs. Run it again to refresh
# the DLLs; a prefix that is already there is not rebuilt.
#
#   WINEPREFIX     the prefix to make (default ~/.wine-demarc)
#   DEMARC_BRANCH  branch to download files/ from when not run from a checkout
set -Eeuo pipefail

WINE_VERSION=11.17
WINE_URL="https://github.com/Kron4ek/Wine-Builds/releases/download/$WINE_VERSION/wine-$WINE_VERSION-amd64-wow64.tar.xz"
FILES_URL="https://raw.githubusercontent.com/sasq64/demarc/${DEMARC_BRANCH:-main}/files"
FILES=(gm.dls tssoft32.acm tsd32.dll dcomp.dll winmm.dll kernelbase.dll dx8vb.dll opengl32.dll)

export WINEPREFIX="${WINEPREFIX:-$HOME/.wine-demarc}"
export WINEARCH=win64
# demarc puts this ahead of PATH itself, so a wine unpacked here needs no setup
WINE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/demarc/wine"
export PATH="$WINE_DIR/bin:$PATH"

here="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

warn() { echo "** $*" >&2; }
has() { command -v "$1" >/dev/null; }

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
    has "$1" && return
    echo "Installing $2"
    install_packages "$2" || warn "Could not install $2, install it yourself"
}

install_wine() {
    has wine && return
    [ "$(uname -m)" = x86_64 ] || { warn "No wine found, and only x86_64 builds are downloaded"; exit 1; }
    echo "Downloading wine $WINE_VERSION to $WINE_DIR"
    mkdir -p "$WINE_DIR"
    curl -fL "$WINE_URL" | tar -C "$WINE_DIR" -xJ --strip-components=1
}

# SteamOS's gstreamer has neither the ASF demuxer nor libav, which WMA soundtracks need
install_steamdeck_gstreamer() {
    local gst="${XDG_DATA_HOME:-$HOME/.local/share}/demarc/gst" v pkg
    [ -f "$gst/libgstlibav.so" ] && return
    echo "Downloading gstreamer plugins"
    mkdir -p "$gst"
    v=$(pacman -Q gstreamer | cut -d' ' -f2)
    for pkg in gst-plugins-ugly:libgstasf.so gst-libav:libgstlibav.so; do
        curl -fL "https://archive.archlinux.org/packages/g/${pkg%:*}/${pkg%:*}-$v-x86_64.pkg.tar.zst" |
            tar -C "$gst" --zstd -x --strip-components=3 "usr/lib/gstreamer-1.0/${pkg#*:}"
    done
}

check_gstreamer() {
    local missing=() plugin
    for plugin in asfdemux avdec_wmav2; do
        gst-inspect-1.0 --exists "$plugin" 2>/dev/null || missing+=("$plugin")
    done
    [ ${#missing[@]} -eq 0 ] && return
    warn "gstreamer plugins not installed: ${missing[*]} (demos with WMA music will be silent)"
    case $SYSTEM in
        arch) warn "Try: sudo pacman -S gst-plugins-ugly gst-libav" ;;
        debian) warn "Try: sudo apt-get install gstreamer1.0-plugins-ugly gstreamer1.0-libav" ;;
    esac
}

# Prints the directory holding demarc's files/: a checkout's, or a downloaded copy
find_files() {
    local dir f
    for dir in "$here/../files" "$here/files"; do
        [ -f "$dir/winmm.dll" ] && { echo "$dir"; return; }
    done
    mkdir -p "$tmp/files"
    for f in "${FILES[@]}"; do
        curl -fsSL -o "$tmp/files/$f" "$FILES_URL/$f" || rm -f "$tmp/files/$f"
    done
    echo "$tmp/files"
}

make_prefix() {
    curl -fsSL -o "$tmp/winetricks" https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks
    chmod +x "$tmp/winetricks"
    local winetricks="$tmp/winetricks"

    # All native D3D
    wineboot -i
    "$winetricks" -q dxvk d3dx9 d3dx10 vkd3d
    "$winetricks" -q d3dx11_42 d3dx11_43 d3dcompiler_42 d3dcompiler_43 d3dcompiler_46 d3dcompiler_47
    # winetricks' d3dcompiler_47 is 8.1 (6.3.9600); 10.0.15063 compiles typed UAV loads (One Of Those Days)
    curl -fsSL https://github.com/lutris/d3d_extras/releases/download/v2/v2.tar.xz |
        tar -xJ -C "$tmp" v2/x32/d3dcompiler_47.dll v2/x64/d3dcompiler_47.dll
    cp "$tmp/v2/x32/d3dcompiler_47.dll" "$WINEPREFIX/drive_c/windows/syswow64/"
    cp "$tmp/v2/x64/d3dcompiler_47.dll" "$WINEPREFIX/drive_c/windows/system32/"
    "$winetricks" -q corefonts
    # MFC42 (Codename Chinadoll)
    "$winetricks" -q mfc42
    # Native msvcp60 from the same download; wine's lacks exports (Come Clean)
    cabextract -q -d "$WINEPREFIX/drive_c/windows/syswow64" -F msvcp60.dll \
        "${XDG_CACHE_HOME:-$HOME/.cache}/winetricks/vcrun6/vcredist.exe"
    # Speech (for Zoom 3)
    "$winetricks" --force -q speechsdk

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
    if [ -f "$files/$1" ]; then
        install -Dm644 "$files/$1" "$WINEPREFIX/drive_c/windows/$2/$1"
    else
        warn "Missing $1: some demos will not work"
    fi
}

install_prefix_files() {
    local files
    files=$(find_files)

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
    # The root filesystem is read-only: cabextract comes in the Deck bundle
    [ -f "$here/cabextract" ] && install -D "$here/cabextract" "$WINE_DIR/bin/cabextract"
    install_steamdeck_gstreamer
    # Run over ssh: wine's windows go to the Deck's screen, and no mono/gecko prompts
    export DISPLAY="${DISPLAY:-:0}"
    export WINEDLLOVERRIDES="mscoree=d;mshtml=d;wineusb.sys=d"
fi
need_tool cabextract cabextract
need_tool bwrap bubblewrap

if [ ! -d "$WINEPREFIX" ]; then
    echo "Making the wine prefix $WINEPREFIX"
    [ "$SYSTEM" = steamdeck ] && echo "Reboot the Deck afterwards if its buttons stop responding."
    trap 'warn "Making $WINEPREFIX failed; remove it and run again"' ERR
    make_prefix
    trap - ERR
else
    echo "Updating the wine prefix $WINEPREFIX"
fi
install_prefix_files
[ "$SYSTEM" = steamdeck ] || check_gstreamer

if has demarc; then
    demarc --check-wine || true
fi
