---
name: wine-demo-debug
description: Debug a Windows demo that fails under wine in demarc — crash, black or white screen, missing DLL function, shader compile error, stuck setup dialog, no sound. Use when a .exe release does not play correctly.
---

# Debugging Windows demos under wine

Each paragraph stands alone; delete one when it stops being true. Add new
findings to `docs/LEARNINGS.md` and, if they are general, here.

## Setup

**Prefix.** Always `WINEPREFIX=~/.wine-demarc`, never `~/.wine` (192 DPI crops
demos to a quarter). Built by `just wine-prefix` (`scripts/mk_wine_prefix.sh`):
DXVK, native d3dx9/10/11 and d3dcompiler, corefonts, mfc42, plus demarc's own
proxy DLLs in `syswow64`. `demarc --check-wine` reports wine, bwrap and prefix.

**Sandbox.** A demarc session runs in a bwrap overlay of the prefix, so nothing
a demo writes survives. `wine_sandbox=false` runs in the real prefix. Plain
`wine` always writes to the real one.

**Wine build.** `~/wine-builds/wine-11.17-amd64-wow64/bin` (wow64-only). 11.16
kills wineserver on packed exes. A multiarch build needs `lib32-libxrandr`, or
32-bit demos log `NtUserChangeDisplaySettings ... returned -2` and stay windowed.

Locating demo. Demo may be referenced by demozoo (id), then the information
can be found in demodb/demozoo.txt. A crashing demo should already be cached
in ~/.cache/demarc/downloads/

## Three ways to run

**1. Through demarc (default).** Headless, never windowed (it opens windows
and plays audio):

```sh
RUST_LOG=retro=warn target/release-fast/demarc demo.exe \
    --headless --no-silence --remote-control script.lua </dev/null
```

`RUST_LOG=retro=warn` shows wine's and Mesa's stderr filtered to missing DLLs,
unimplemented functions, GL and GLSL errors; `retro=debug` shows all of it. A
`WINEDEBUG` set for demarc replaces the default `fixme-all`. `--no-silence` is
needed for gamescope's, autodlg's and Lua `print` output. Meta goes in with
`-x key=value` (splits on commas). In the script, `wait_frames(60)` between
`screenshot()` and `quit()`; shoot several frames apart before calling a window
black (a launcher can take 360 frames). `audio_level()` tells if sound plays.

**2. Plain wine.** For a crash that is wine's own, skip demarc:

```sh
cd <demo dir> && WINEPREFIX=~/.wine-demarc WINEDEBUG=+loaddll wine demo.exe
```

demarc's launch path adds things plain wine lacks: autodlg, DLL overrides for
DLLs shipped beside the exe, `allow_glsl_120_subset_in_110`, the session size,
override patches. If it works here and not in demarc, the difference is there.

**3. Under gamescope (rare).** When the session itself matters (mode switches,
fullscreen, front-buffer drawing) or a tool needs a display without a window:

```sh
WINEDEBUG=+ddraw gamescope --backend headless -w 1024 -h 768 -- wine demo.exe
```

Never export Mesa overrides (`force_glsl_version`) where gamescope inherits
them: they crash Xwayland's glamor. Put them after `--` with `env`.

**Local core.** The downloaded gamescope core can predate an option:
`DEMARC_CORE_DIR=$PWD/external/gamescope/build-lr/src` (`just gs <file>`).
When every Windows demo fails with a bare `retro_load_game(...) failed`, run
the core directory's `./gamescope --version` for the real error.

**Overrides in tests.** Fixes live in `system/overrides.toml`, keyed on the db
file name: a test db must be called `demozoo.txt` / `csdb.txt` or its entry is
not applied. Edit `system/` and rebuild; `~/.cache/demarc/system` is rewritten
at start. A `wine_dialog_res:pick` field on the db line leaves the dialog up.

## Tools

**WINEDEBUG channels.** `+loaddll` (import list, also of packed exes), `+relay`
(builtin DLLs only; `grep 'ret=004'` for the exe's own calls), `+opengl`,
`+d3dx,+d3d9` (grep `stub`), `+ddraw`, `warn+ddraw`. Default hides `warn:`.

**winedbg.** `winedbg --file cmds "Z:\\path\\x.exe"` with `cont`, `info reg`,
`bt`, `x/Nx` in `cmds`; copy the exe to a space-free path. For a packed exe,
dump the unpacked code from `/proc/<pid>/mem` while it runs and
`objdump -b binary --adjust-vma=0x400000`.

**wineserver.** `wineserver -f -p -d1` logs the request stream when `wine`
exits 0 and nothing ran.

**Dialogs.** `wine demarc-autodlg.exe --list` (from `system/win/`) dumps a
running demo's setup dialog. A dialog drawn in GL has no controls: try
`press_key(Key.Enter)` or an override `events`/`click` before writing code.

**NVIDIA reference.** `ssh ripper` has an RTX 4070 and a demarc checkout; wine
is not on its ssh `PATH`. For black-on-Mesa, run the same `+opengl` trace on
both and diff call-name histograms before reading frames.

**Test exes.** A few-KB repro builds without mingw: `clang
--target=i686-w64-windows-gnu -nostdlib -fuse-ld=lld
-I<wine>/include/wine/windows -Wl,--entry=_start@0` against
`<wine>/lib/wine/i386-windows/*.a`.

## Symptoms

**Missing or stubbed DLL function.** `+loaddll` and `+relay` first; a
`GetProcAddress() retval=00000000` is the missing export. A UPX-packed DLL
shows only `"X.dll" failed to initialize`. Fixes, in order: winetricks native
DLL in the prefix; `wine_dll_overrides` (wine's syntax, `d3dx9_37=n`); a proxy
DLL like those in `tools/` (jumps into wine's builtin via `WINEDLLDIR<n>`). A
real DLL copied over wine's fake in `syswow64` loads without an override.
Only some d3dx9 versions are native; others still hit wine's stubs.

**Crinkler / packed 4k.** Imports are resolved by walking export tables, so a
wine export list that differs from Windows breaks silently. Existing proxies:
`winmm` (ordinal layout, WAVEHDR copies), `kernelbase` (no forwarders),
`opengl32` (GL 1.1 names from `wglGetProcAddress`, alpha bits), `dx8vb`
(`VB_D3DX*` math). Odd DLL casing in `+loaddll` is deliberate. See
`docs/CRINKLER.md`. The exe cannot be patched; an unpacked one can, with an
override `patch`.

**GLSL compile errors (white or black GL intro).** Read Mesa's errors at
`RUST_LOG=retro=warn`; `MESA_DEBUG=1` shows "program not linked" when
`glCreateShaderProgramv` hides it. `MESA_SHADER_DUMP_PATH=<dir>` writes each
shader as `FS_<hash>.glsl`. Fixes: `wine_glsl_version=130` for unversioned
shaders using `texture()`/`round()`; `wine_glsl_120_subset` (on by default) for
int/float mixing; for NVIDIA-only GLSL, `wine_shader_read` plus a `patch`
providing the corrected `FS_<hash>.glsl` (see Surveillance, `zoo.155010`).

**HLSL compile errors.** Use the builtin compiler to see return codes
(`WINEDLLOVERRIDES=d3dcompiler_47=b` with `+relay`) and
`VKD3D_SHADER_DUMP_PATH=<dir>`: a `-source.hlsl` with no `-target.dxbc` is the
one that failed.

**GL function pointer is NULL (page fault at 0).** wine's `wglGetProcAddress`
refuses names whose extension is not on the context. `wine_gl_compat` asks
Mesa for a 4.6 compatibility context. Functions above the context version are
still refused.

**Black screen with music.** Check `+opengl` for `glDrawBuffer buf 1028` or a
single-buffered pixel format: front-buffer drawing never reaches the session.
wined3d's ddraw draws that way; `wine_cnc_ddraw=true` for 2D ddraw demos.
Also check `GL_POINT_SMOOTH` (radeonsi draws nothing; zink does) and NV-only
extensions such as `glLoadProgramNV`.

**Black or instant exit with a small `wine_res`.** Grep the `--no-silence` log
for `Setting display mode` / `Failed to change display mode`: a mode larger
than the session cannot be set. A demo that never calls `glViewport`, or draws
in a corner, needs `wine_res` set to its own resolution. `wine_desktop=true`
is for demos that do not survive a mode switch.

**White or washed-out D3D9.** `WINEDEBUG=+d3dx,+d3d9`, grep `stub`. DXVK is the
prefix default; `wine_dll_overrides=d3d9=b` tries wined3d, which forgives more
invalid API use.

**Crash at an ntdll address before the demo runs.** `c0000005` with `+loaddll`
stopping after kernel32 is wine's import walk on an overlapping 1k header;
autodlg works around the known case.

**Message box before any window.** `WINEDEBUG=+relay wine demo.exe 2>&1 | grep
'ret=004'` lists the calls up to the `MessageBoxA`; often a failed
`SetCurrentDirectory` or file open, not a codec.

**Timing asserts and races.** `wine_cpus=N` limits the CPU count but may only
make a race rarer; run several times before calling it fixed.

**Fault in msvcrt `memset`/`memcpy` that vanishes under `+heap` or
winedbg.** The demo overran an earlier heap block into wine's free-block
header; trace `+virtual,+seh` (layout unchanged) and the relay's `new`
sizes. Fix the unpacked exe and ship a bsdiff `source` patch (Akkordarbeit,
`zoo.20036`).

**Audio.** A crash in wine's winmm thread: check whether the `WAVEHDR` is a
stack address. winmm MIDI is silent without a synth and soundfont. Odd ACM
codecs need installing in the prefix (`docs/WINE.md`).

## Further reading

`docs/WINE.md` (prefix tweaks, known failures), `docs/GAMESCOPE.md` (core
options, sandbox), `docs/CRINKLER.md`, `docs/WINE_RES_FIX.md`,
`tools/*/README.md`.
