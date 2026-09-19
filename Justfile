
test:
    cargo test

clippy:
    cargo clippy

coverage:
    cargo llvm-cov --ignore-run-fail --html --open

coverage_text:
    cargo llvm-cov ---ignore-run-fail

RUST_SYSROOT := `rustc --print sysroot`

perf:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib target/debug/client

cachegrind_debug:
  CARGO_MANIFEST_DIR=. LD_LIBRARY_PATH=target/debug/deps:{{RUST_SYSROOT}}/lib/rustlib/x86_64-unknown-linux-gnu/lib valgrind --tool=cachegrind target/debug/client

cachegrind:
  CARGO_MANIFEST_DIR=. valgrind --tool=cachegrind target/release-fast/demarc

run file="testdata/amiga/rebels.adf":
    cargo run --profile release-fast -- --shuffle {{file}}

gb:
    cargo run --profile release-fast -- --scale 4 testdata/gb/nightmode.gb

c64:
    cargo run --profile release-fast -- testdata/c64/quantum.prg

ami:
    cargo run --profile release-fast -- testdata/amiga/rebels.adf

iff:
    cargo run --profile release-fast -- -C testdata/test.iff

royale file="testdata/amiga/rebels.adf":
    cargo run --profile release-fast -- --shuffle --slangp slang-shaders/crt/crt-royale.slangp {{file}}

# Build the 86Box libretro core out of external/86box. 86Box is GPLv2 and is
# not shipped with demarc; point demarc at the result with DEMARC_CORE_DIR, or
# copy it into <system dir>/cores.
#
# BIOS ROMs are not included and never will be: put them under
# <system dir>/86box/roms/ in 86Box's own layout (see docs/86BOX.md).
86box-core:
    cmake -S external/86box -B external/86box/build-lr -G Ninja -DLIBRETRO=ON \
        -DCMAKE_BUILD_TYPE=Release -DRTMIDI=OFF -DFLUIDSYNTH=OFF -DMUNT=OFF -DSOUNDCANVAS=OFF
    ninja -C external/86box/build-lr
    @echo "core at external/86box/build-lr/src/86box_libretro.so"

# Run an 86Box machine config through the locally built core.
pc file:
    DEMARC_CORE_DIR={{justfile_directory()}}/external/86box/build-lr/src \
        cargo run --profile release-fast -- {{file}}


GAMESCOPE := "libretro/gamescope"

# Needs meson, vulkan-headers, glslang and the wlroots build deps, plus the
# submodules: git -C external/gamescope submodule update --init --recursive.
# Point demarc at the result with DEMARC_CORE_DIR; the core finds the compositor
# beside itself in the build directory. See docs/GAMESCOPE.md.
#
# Build the gamescope libretro core: the patched compositor and the core that drives it.
gamescope-core:
    meson setup --reconfigure {{GAMESCOPE}}/build-lr {{GAMESCOPE}} \
        -Dbuildtype=release -Denable_openvr_support=false -Denable_tests=false \
        -Denable_gamescope_wsi_layer=false -Dpipewire=disabled \
        -Davif_screenshots=disabled -Dforce_fallback_for=libliftoff,vkroots
    ninja -C {{GAMESCOPE}}/build-lr src/gamescope src/gamescope_libretro.so
    @echo "core at {{GAMESCOPE}}/build-lr/src/gamescope_libretro.so"

# `--no-silence` matters: without it gamescope's and wine's diagnostics go to
# /dev/null along with the cores'.
#
# Run a Windows demo against a locally built gamescope core.
gs file:
    DEMARC_CORE_DIR={{justfile_directory()}}/{{GAMESCOPE}}/build-lr/src \
        cargo run --profile release-fast -- --no-silence {{file}}

# Same, for an HTML/JS release through an undecorated Chrome. WebSystem claims
# the page, so nothing extra has to be said on the command line.
gs-web page:
    DEMARC_CORE_DIR={{justfile_directory()}}/{{GAMESCOPE}}/build-lr/src \
        cargo run --profile release-fast -- --no-silence {{page}}

install:
    cargo build --release
    sudo cp target/release/demarc /usr/local/bin

# What a `git tag v<version>` push would produce (no build).
release-check:
    dist plan

# Build this host's release artifacts into target/distrib, as CI would.
release-local:
    dist build --artifacts=local

HOME := x'${HOME}'
ZOLA := HOME / "projects/docs/minnberg"

# The dialog driver that runs inside wine alongside a Windows demo (source in
# tools/autodlg). It is a checked-in binary, so a change to it only reaches
# demarc once this has been run and system/win/demarc-autodlg.exe committed.
# `+crt-static` so the driver depends on nothing but wine's own kernel32 and
# user32: it has to start in whatever state the demo's prefix happens to be in.
autodlg:
    cd tools/autodlg && RUSTFLAGS="-C target-feature=+crt-static" cargo xwin build --release --target x86_64-pc-windows-msvc
    cp tools/autodlg/target/x86_64-pc-windows-msvc/release/demarc-autodlg.exe system/win/

# winmm.dll with the Windows export layout (source in tools/winmm). Checked in
# like the dialog driver; needs clang, lld-link and llvm-dlltool.
winmm:
    python3 tools/winmm/build.py files/winmm.dll

# dcomp.dll that gives DirectComposition demos a swapchain (source in
# tools/compshim). Checked in, and installed in the prefix by scripts/setup-wine.sh.
compshim:
    cd tools/compshim && cargo xwin build --release --target x86_64-pc-windows-msvc
    cp tools/compshim/target/x86_64-pc-windows-msvc/release/dcomp.dll files/

# `-mssse3 -maes`: the vendored unrar C++ sources (unarc-rs -> unrar -> unrar_sys)
# tag their SSE/AES-NI routines with `__attribute__((target(...)))` only under
# `#ifdef __GNUC__`, which clang-cl doesn't define, so clang rejects the
# intrinsics unless the features are on for the whole translation unit. Both are
# runtime-dispatched inside unrar; SSSE3/AES-NI are a 2006/2010 baseline.
# scripts/prepare-xwin.sh patches the SDK cache -- see the comments there.
win:
    ./scripts/prepare-xwin.sh
    CXXFLAGS="-mssse3 -maes" cargo xwin build --release --target x86_64-pc-windows-msvc
    cp target/x86_64-pc-windows-msvc/release/demarc.exe {{ZOLA}}/static/dl/

site:
    cp demarc.md {{ZOLA}}/content/
    zola -r {{ZOLA}} build
    rsync -avz {{ZOLA}}/public/ sasq@minnberg.se:/var/www/html/

# Hyprland >= 0.5x parses its config as Lua, and `hyprctl keyword` only works
# with the legacy parser ("keyword can't work with non-legacy parsers. Use
# eval."), so drive the monitor through `hyprctl eval` instead.
pal:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "2880x1920@50", position = "auto", scale = 2 })'

ntsc:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "2880x1920@60", position = "auto", scale = 2 })'

# Back to the panel's native refresh rate.
native:
    hyprctl eval 'hl.monitor({ output = "eDP-1", mode = "preferred", position = "auto", scale = 2 })'


PRE := HOME / ".wine-demarc"

wma-audio:
    sudo pacman -S gst-plugins-ugly

wine-prefix: wma-audio
    #curl -fsSL -o winetricks https://raw.githubusercontent.com/Winetricks/winetricks/master/src/winetricks
    #chmod +x winetricks
    rm -rf {{PRE}}
    WINEPREFIX={{PRE}} wineboot -i
    WINEPREFIX={{PRE}} ./winetricks dxvk d3dx9 d3dx10
    # For Zoom3
    WINEPREFIX={{PRE}} ./winetricks --force -q speechsdk
    # For Texas / Keyboarders (Safe version)
    mkdir -p "{{PRE}}/drive_c/users/Public/Music/Sample Music"
    # For 1995 / Kewlers
    WINEPREFIX={{PRE}} wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f
    # Make sure we have 1:1 logical/physical pixel mapping
    WINEPREFIX={{PRE}} wine reg add 'HKCU\Control Panel\Desktop' /v LogPixels /t REG_DWORD /d 96 /f
    # For Panic Room / FLT
    install -Dm644 files/tssoft32.acm {{PRE}}/drive_c/windows/syswow64/tssoft32.acm
    install -Dm644 files/tsd32.dll    {{PRE}}/drive_c/windows/syswow64/tsd32.dll
    WINEPREFIX={{PRE}} wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
    WINEPREFIX={{PRE}} wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d tssoft32.acm /f
    install -Dm644 files/gm.dls {{PRE}}/drive_c/windows/syswow64/drivers/gm.dls
    install -Dm644 files/gm.dls {{PRE}}/drive_c/windows/system32/drivers/gm.dls
    WINEPREFIX={{PRE}} wine reg add 'HKLM\Software\Microsoft\DirectMusic' /v GMFilePath /t REG_SZ /d 'C:\windows\system32\drivers\gm.dls' /f

wine32-fix:
    # Needed for fullscreen switch with 32bit wine
    sudo pacman -S lib32-libxrandr lib32-libxi
