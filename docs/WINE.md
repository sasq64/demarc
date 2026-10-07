# WINE INFO

demarc runs every Windows release in `~/.wine-demarc` (`PREFIX_DIR` in
`src/wine.rs`) — so that is the prefix every command below wants `WINEPREFIX`
pointed at. Deliberately not the user's
own `~/.wine`: besides what a demo may install into it, a personal prefix on a
hidpi screen carries `LogPixels` 192, and wine then hands a non-DPI-aware demo
a screen half the size and blows the result up — the top-left quarter of the
picture, at double size, with nothing anywhere saying why. demarc puts its own
prefix back to 96 when it finds it moved.

## DEMO SETUP

### Wine modifications

#### Use GLX instead of EGL

`wine reg add 'HKCU\Software\Wine\X11 Driver' /v UseEGL /d N /f`

GLX is older and more tested.
For running 1995 / Kewlers on Nvidia hardware

#### DXVK

`winetricks dxvk`

winertricks installation warns, but works.

Fixes white screen in elevated

### Installing tssoft32.acm for Panic Room

```sh
cp /home/sasq/projects/demarc/panic/tssoft32.acm ~/.wine-demarc/drive_c/windows/syswow64/tssoft32.acm
wine reg add 'HKLM\Software\Wow6432Node\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
wine reg add 'HKLM\Software\Microsoft\Windows NT\CurrentVersion\Drivers32' /v msacm.tssoft32 /t REG_SZ /d 'tssoft32.acm' /f 2>&1 | tail -2;
```


## Known failures

From the "Black screen" playlist, 2026-10. Fixed ones are in `overrides.toml`
or the code; these are not:

- **Atlas** (zoo 202450). Dialog now works (see `fix_resource_dir`), then one
  pixel shader (godrays, `#include "16" "15" "14"` where 14 includes 15 and 16
  again) fails to compile with both the prefix's d3dcompiler_47 (6.3.9600, 2013)
  and wine's vkd3d, and the intro dereferences the NULL blob at 0x403721.
  Likely needs a Windows 10 d3dcompiler_47 that honours `#pragma once`.
- **Transformer 3** (zoo 202844). Java/OpenRNDR asks for a 3.3 core context and
  calls `glBlendEquationi` (GL 4.0). wine's `wglGetProcAddress` refuses entry
  points above the context version (`Extensions required for glBlendEquationi
  not supported`); Windows drivers hand them out. Would need the GL version
  hint in the jar patched.
- **Antimoney** (zoo 49452). 1024x768x8 DirectDraw flip chain: wined3d (GL and
  Vulkan) cannot make a `P8_UINT` render target, and `renderer=gdi` rejects
  video-memory surfaces, so the primary is NULL and it crashes at 0x40CA8C.
  A ddraw wrapper such as cnc-ddraw is the likely fix.
- **Codename Chinadoll** (zoo 22636) plays, but about one start in three stays
  black after its menu, and key delivery into the session is itself not
  reliable (Lua keys and override events alike sometimes do nothing).
