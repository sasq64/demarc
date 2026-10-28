---
name: wine-demo-debug
description: Debug a Windows demo that fails under wine in demarc — crash, black or white screen, missing DLL function, shader compile error, stuck setup dialog, no sound. Use when a .exe release does not play correctly.
---

# Debugging Windows demos under wine

Each paragraph stands alone; delete one when it stops being true. Add new
findings to `docs/LEARNINGS.md` and, if they are general, here.

## Setup

**Prefix.** Always `WINEPREFIX=~/.wine-demarc`, never `~/.wine` (192 DPI crops
demos to a quarter). Built by `demarc --setup-wine` / `just wine-prefix` (`system/win/setup-wine.sh`):
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

Plain wine plays sound on the host (no capture); keep runs short.
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
dump the unpacked code from `/proc/<pid>/mem` while it runs (from
the script that started gamescope, since ptrace_scope only lets ancestors read) and
`objdump -b binary --adjust-vma=0x400000`.

**wineserver.** `wineserver -f -p -d1` logs the request stream when `wine`
exits 0 and nothing ran.

**Dialogs.** `wine demarc-autodlg.exe --list` (from `system/win/`) dumps a
running demo's setup dialog. A dialog drawn in GL has no controls: try
`press_key(Key.Enter)` or an override `events`/`click` before writing code.

**Reading back frames.** A temporary hook in the opengl32 proxy (e.g. on
`glEnd`) that `glReadPixels` a row of framebuffer 0 and prints state with
`__wine_dbg_output` shows whether a frame was drawn. Swaps and pixel formats
set through gdi32 bypass the proxy. `x11grab` of gamescope's Xwayland does
not see GL content, and demarc's wine log drops repeated lines.

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
`winmm` (ordinal layout, WAVEHDR copies), `kernelbase` (no forwarders, plus
Windows-only names in `EXTRA`),
`opengl32` (GL 1.1 names from `wglGetProcAddress`, alpha bits, pbuffer
`wglBindTexImageARB` on NVIDIA, accumulation buffer), `dx8vb`
(`VB_D3DX*` math). Odd DLL casing in `+loaddll` is deliberate. See
`docs/CRINKLER.md`. A `+relay` call to `AccessCheck` with `ret=004...` is a
name kernelbase lacks (Kindercrasher's `OpenFile`): add it to `EXTRA`. A 4k
frozen at 100% CPU: `perf record -p` shows the hot loop, and a scan of
`/proc/<pid>/mem` (from gamescope's parent) for its stack frame shows the
bad locals. The exe cannot be patched; an unpacked one can, with an
override `patch`.

**GLSL compile errors (white or black GL intro).** Read Mesa's errors at
`RUST_LOG=retro=warn`; `MESA_DEBUG=1` shows "program not linked" when
`glCreateShaderProgramv` hides it. `MESA_SHADER_DUMP_PATH=<dir>` writes each
shader as `FS_<hash>.glsl`. Fixes: `wine_glsl_version=130` for unversioned
shaders using `texture()`/`round()`; `wine_glsl_120_subset` (on by default) for
int/float mixing; for anything else an override `glsl` patch edits the
source by regex before it is compiled (fr-080 `zoo.4701`, Surveillance
`zoo.155010`); `retro=debug` logs `opengl32: shader <hash>` for each.
`__GLX_VENDOR_LIBRARY_NAME=mesa LIBGL_ALWAYS_SOFTWARE=1` gets Mesa's errors and
dumps on an NVIDIA machine; `-x wine_glsl_patches=...` tries a patch without an
override. NVIDIA's compile error only shows in the demo's own message
box: `WINEDEBUG=fixme-all,+msgbox` prints its text. An `int3` (`0x80000003`)
right after `glGetShaderiv` is an assert: the info log is in winedbg at the
fault, the source in the unpacked image; glslang over every string in it finds
the rest (On, `zoo.136151`). When the sources are not plain strings, iterate
under software Mesa with dumps, one patch per error (Hologram, `zoo.119170`).
A `match` is a regex.

**HLSL compile errors.** Use the builtin compiler to see return codes
(`WINEDLLOVERRIDES=d3dcompiler_47=b` with `+relay`) and
`VKD3D_SHADER_DUMP_PATH=<dir>`: a `-source.hlsl` with no `-target.dxbc` is the
one that failed. A NULL-blob fault right after `D3DCompile` with no error
blob asked for: compile the dumped source with each DLL from a test exe and
print the errors; the prefix needs a Windows 10 `d3dcompiler_47` (10.0.x).

**Float pbuffers.** `wglChoosePixelFormatARB` with `WGL_TYPE_RGBA_FLOAT_ARB`
finds nothing under Xwayland; the opengl32 proxy emulates them with FBOs
(`zoo.8645`). An ATI-era demo that checks for `GL_NV_*` may take an NV path
whose files were never shipped: hide the name with an override `patch`.

**GL function pointer is NULL (page fault at 0).** wine's `wglGetProcAddress`
refuses names whose extension is not on the context. `wine_gl_compat` asks
Mesa for a 4.6 compatibility context. Functions above the context version are
still refused.

**Java demos.** A `runtime/` + `app/*.cfg` release is javapackager: run
`runtime/bin/java.exe -cp app/<jar> <app.mainclass>` for the stack trace. A GL
4.x call on a 3.3 core context dies on NVIDIA only; bump the GLFW hint in the
jar (`zoo.202844`).

**Black screen with music.** Check `+opengl` for `glDrawBuffer buf 1028` or a
single-buffered pixel format: front-buffer drawing never reaches the session
(the opengl32 proxy remaps a single-buffered `wglSetPixelFormat`).
wined3d's ddraw draws that way; `wine_cnc_ddraw=true` for 2D ddraw demos. SDL 1.2 demos (`SDL.DLL` beside the exe) are ddraw too: black and
silent without it (Dose 2, `zoo.8540`).
Also check `GL_POINT_SMOOTH` (radeonsi draws nothing; zink does) and NV-only
extensions such as `glLoadProgramNV`. Black on NVIDIA only, first frame
drawn: the window's depth is not kept across a swap, and a demo that cleared
it once fails its depth test (Axxelerate, `zoo.31450`, patched to clear it).

**D3D8/9 2D quads show one colour or a zoomed corner.** XYZRHW quads drawn
with texgen (`TEXCOORDINDEX` camera-space) or a texture matrix left on from a
3D pass: Windows ignores both for pre-transformed vertices, DXVK applies the
texgen. Dump the vertices and states at the draw (winedbg breakpoint on the
device vtable). Fix in the unpacked exe; Nature v2.0 (`zoo.8340`) routes each
`SetVertexShader(XYZRHW)` through a stub that zeroes both.

**NaN uniforms, frames slowing to seconds.** `+opengl` shows a
`glUniform*` value of `-nan(ind)`: the intro leaks x87 stack slots per frame
and `fild` overflows. The opengl32 proxy's `emms` after `wglSwapBuffers`
covers it (Yama, `zoo.398638`).

**Black or instant exit with a small `wine_res`.** Grep the `--no-silence` log
for `Setting display mode` / `Failed to change display mode`: a mode larger
than the session cannot be set. A `returned -2` with a mode that fits is the
refresh rate (`WINEDEBUG=+system` shows `dmDisplayFrequency`): the session has
640x480 at 59 Hz only, so patch the demo's config to rate 0 (`zoo.46672`). A demo that never calls `glViewport`, or draws
in a corner, needs `wine_res` set to its own resolution. TinyPTC demos also need
`wine_cnc_ddraw=true`, or `wine_res` turns them black (Bakkslide 7, `zoo.45277`). `wine_desktop=true`
is for demos that do not survive a mode switch.

**D3D9 `CreateDevice` fails, then a NULL fault.** Check the present
parameters for `D3DMULTISAMPLE_NONMASKABLE`: DXVK makes quality q into `1 << q`
samples. wined3d with `wine_d3d_config = "SampleCount=0"` (Spilled,
`zoo.51354`). A draw-once D3D11 intro black under DXVK: try
`wine_dll_overrides=d3d11=b;dxgi=b` (Night Time, `zoo.155141`).

**White or washed-out D3D9.** `WINEDEBUG=+d3dx,+d3d9`, grep `stub`. DXVK is the
prefix default; `wine_dll_overrides=d3d9=b` tries wined3d, which forgives more
invalid API use.

**Crash at an ntdll address before the demo runs.** `c0000005` with `+loaddll`
stopping after kernel32 is wine's import walk on an overlapping 1k header;
autodlg works around the known case.

**Message box before any window.** `WINEDEBUG=+relay wine demo.exe 2>&1 | grep
'ret=004'` lists the calls up to the `MessageBoxA`; often a failed
`SetCurrentDirectory` or file open, not a codec. A `CreateFileA("\\tNN.")` that fails is
the old CRT's `tmpfile()` writing to the drive root; the sandbox maps the
release directory to its own drive for it, plain wine does not.

**Unreal Engine 4 asks for `GlobalShaderCache-GLSL_150.bin`.** It fell back to
OpenGL because the prefix says Windows XP (left by winetricks' `speechsdk`);
an `AppDefaults\<exe>` `Version=win10` key in `setup-wine.sh` fixes it.

**Old GL demo dies right after `glGetString`/`glGetIntegerv`.** Modern
values overflow 2000-era assumptions: `GL_EXTENSIONS` into a fixed buffer,
a texture of `GL_MAX_TEXTURE_SIZE` squared. Patch the call in the unpacked
exe (Adelanto, `zoo.46032`); env vars do not shorten wine's extension string.
A "no OpenGL 1.x driver" error can be a `strstr(GL_VERSION, "1.")` that
hits Mesa's `arch1.1` (Arise, `zoo.31637`).

**Picture stalls for seconds, music plays on.** Count new frames per second
(a temporary log in the gamescope core's `retro_run`; demarc's own calls stay
at 60). Stalls with the GPU idle and the render thread at thousands of
voluntary context switches a second are lock contention: the futex address
from `/proc/<pid>/task/<tid>/syscall`, looked up in `maps` and `nm ntdll.so`,
names the lock. `fd_cache_mutex` is ntsync's handle cache: `wine_ntsync=false`
(Ziphead, `zoo.136314`). Find the pid with `ps -eo pid,comm`, never `pgrep -f`.

**Timing asserts and races.** `wine_cpus=N` limits the CPU count but may only
make a race rarer; run several times before calling it fixed.

**Fault in msvcrt `memset`/`memcpy` that vanishes under `+heap` or
winedbg.** The demo overran an earlier heap block into wine's free-block
header; trace `+virtual,+seh` (layout unchanged) and the relay's `new`
sizes. Fix the unpacked exe and ship a bsdiff `source` patch (Akkordarbeit,
`zoo.20036`).

**Wrong font or missing glyphs in GDI-drawn text.** `WINEDEBUG=+font`, grep
`HfontCreate` and `select_font Chosen`: a face missing from the prefix falls
back to Arial. Segoe UI maps to Selawik through
`HKCU\Software\Wine\Fonts\Replacements` (Epsilon, `zoo.23405`).

**Picture frozen on frame one, music plays.** Count the exe's calls with
`+relay` and `grep 'ret=004'`: a flood of failing `mciSendCommandA` is a
position poll from a thread that did not open the device. The winmm proxy runs
all MCI on one thread for it (Lasse Reinbong '96, `zoo.198069`).

**Audio.** A crash in wine's winmm thread: check whether the `WAVEHDR` is a
stack address. winmm `midiOut*` plays through the winmm proxy's DirectMusic synth
(`gm.dls`); `midiStream*` is still silent. Odd ACM
codecs need installing in the prefix (`docs/WINE.md`).

## Further reading

`docs/WINE.md` (prefix tweaks, known failures), `docs/GAMESCOPE.md` (core
options, sandbox), `docs/CRINKLER.md`, `docs/WINE_RES_FIX.md`,
`tools/*/README.md`.
