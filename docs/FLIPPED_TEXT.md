# Vertically flipped text in PC demos (unsolved)

Some Windows demos draw their text mirrored about the horizontal axis (`y = -y`)
— letter order is preserved, only each line is upside down. `pcdemo/wrath`
(Wrath / Matt Current) is the reference case: the title *THE GRAND MECHANISM*
appears at ~40 s, mirrored.

This is a log of what has been ruled out, so the next attempt does not redo it.
**Not solved yet** — the last open lead is at the bottom.

## The symptom, measured

The titles are pre-rendered 1280x720 JPEGs inside `booty.fs`, not runtime text.
Extracted `overlay*.jpg`: white condensed caps on black, stored **upright**,
bright rows 312..393 of 720.

On screen (demarc screenshot, 1280x720) the same text occupies rows **327..409**
— the mirror of 312..393 is 326..407. So it is a clean full-image vertical flip,
not a per-glyph one, and not a shifted/offset sample.

## Ruled out

| Suspect | Test | Result |
|---|---|---|
| Glyph rasterisation (`ID3DXFont`) | `WINEDEBUG=+font` over 80 s and 260 s | the demo process never creates a GDI font at all; the text is a JPEG |
| wine's GDI glyph path in general | 32-bit test: top-down 32bpp DIB section + `CreateFontIndirectA` + `ExtTextOutW`, dump rows | "F" comes out upright — this is the exact layout native `d3dx9`'s `ID3DXFont` uses (`D3DX9_40.dll` builds a `biHeight = -h`, 32bpp, BI_RGB DIB at `0x101ea92e`) |
| demarc / gamescope compositing | plain `wine explorer /desktop=…`, no demarc, screenshot with `grim` | identical flip |
| DXVK | `-x wine_dll_overrides=d3d9=b` (wine's builtin wined3d) | identical flip |
| wine's D3D9 orientation | purpose-built 32-bit D3D9 probe reading its own back buffer with `GetRenderTargetData` | all four probes **OK** on both DXVK and wined3d: `Clear`→back buffer; `Clear`→RT + `StretchRect`; RT sampled by an XYZRHW textured quad → back buffer; that same quad → a second RT |
| Presentation to the window | `grim` of the simple probe | red top / blue bottom / green left, as drawn |
| `vPos` / `vFace` handling | scanned all 56 D3D9 shader blobs in `booty.fs` for `MISCTYPE` declarations | zero — no shader uses either |
| The demo's image loader, in wine | 32-bit repro of the exact path (below) on the real `overlay*.jpg` | bright rows **312..393** — upright, matching the source |

## What the demo actually does

Found with a relay trace filtered to `kernel32.GetProcAddress`
(`HKCU\Software\Wine\Debug` `RelayInclude` = `kernel32.GetProcAddress`,
`WINEDEBUG=+relay`), which lists every symbol UPX resolves at startup.

It does **not** use D3DX for images. From `d3dx9_40` it resolves only
`D3DXCreateTexture` (an empty texture), `D3DXCreateCubeTexture`,
`D3DXGetShaderConstantTable`, the `D3DXMatrix*`/`D3DXQuaternion*`/`D3DXVec*`
maths, and `D3DXCreateFontA`.

Images are decoded through COM instead:

1. `CreateStreamOnHGlobal` + `OleLoadPicture` → `IPicture`
   (`WINEDEBUG=+olepicture` shows `OLEPictureImpl_SetBitmap width 1280, height 720, bpp 24`
   twenty times — one per overlay — then `OLEPictureImpl_get_Handle`; **no**
   `OLEPictureImpl_Render`).
2. `IPicture::get_Handle` → `HBITMAP`, `GetObjectA` for its dimensions.
3. A **top-down 24bpp DIB section** per image: `WINEDEBUG=+bitmap` shows exactly
   20 × `NtGdiCreateDIBSection format (1280,-720), planes 1, bpp 24, BI_RGB`,
   plus one matching every other texture size in the archive
   (1024x1024 ×3, 512x512 ×10, 256x256 ×7, …).
4. The bits are uploaded to a `D3DXCreateTexture` texture via `LockRect` per mip
   level.

Rendering (from `WINEDEBUG=+d3d9` traces): a deferred renderer, MRT up to 4,
twelve 1280x720 `A16B16G16R16F` render targets. **Every fullscreen pass shares
one vertex buffer** (stride 20, FVF `0x102` = `XYZ|TEX1`, drawn as a 2-triangle
strip), so the overlay quad geometry is identical to every post-process pass.
The overlay texture is bound to **sampler stage 2** of a composite pass whose
stage 0 is a render target and stage 1 a 512x512 texture. No `SetTransform`, no
`SetViewport`, no `StretchRect` anywhere.

## The contradiction to resolve

The pixels load upright, the quad is shared with passes that are not flipped,
and D3D9 is correct in every orientation probe — yet the composite is mirrored.
So the demo itself must be uploading or sampling the overlay flipped, and on
Windows something in the same sequence comes out the other way up.

Best remaining hypothesis: the demo **branches on the `HBITMAP` format returned
by `OleLoadPicture`**. Wine returns a 24bpp DDB for a colour JPEG; Windows
normally returns a screen-compatible DDB (32bpp). A 24bpp branch that reads the
rows bottom-up would produce exactly this, and would be dead code on Windows.
Worth checking whether the flip disappears with a 16/32bpp desktop depth in the
prefix, or whether wine's `OLEPictureImpl_SetBitmap` should be producing a
screen-depth DDB rather than a 24bpp one.

### Next step that was in progress

Disassemble the demo's loader. `wrath.exe` is UPX-packed, so it has to be read
out of the running process:

- `/proc/<pid>/mem` is blocked by `kernel.yama.ptrace_scope`.
- Plan: a small 32-bit Windows helper using `CreateToolhelp32Snapshot` +
  `OpenProcess` + `ReadProcessMemory` to dump `0x400000..0x480000` while the
  demo runs — wine mediates this through the wineserver, no ptrace needed.
- Then find the `CreateDIBSection` / `LockRect` call sites and read the row loop.

## Probe programs

All the 32-bit probes were built with the recipe in `tools/winmm/build.py`:
`clang --target=i686-pc-windows-msvc -ffreestanding -fno-builtin`, import libs
from hand-written `.def` files via `llvm-dlltool -m i386 -k`, linked with
`lld-link /nodefaultlib /safeseh:no` (needs an `int _fltused = 0;` and a
`mainCRTStartup` that calls `__getmainargs`). No Windows SDK is involved — every
struct and prototype is declared in the source. They lived in the session
scratchpad and are gone; rebuild if needed:

- **DIB text probe** — top-down 32bpp DIB section, `CreateFontIndirectA`,
  `ExtTextOutW`, print the rows as ASCII art.
- **D3D9 orientation probe** — `Direct3DCreate9`, `CreateDevice`, paint a red top
  band and a blue bottom band by four different routes, then
  `CreateOffscreenPlainSurface` + `GetRenderTargetData` + `LockRect` and print
  the sampled colours. Uses raw vtable slots: `IDirect3D9::CreateDevice` = 16;
  `IDirect3DDevice9` `Present` 17, `GetBackBuffer` 18, `CreateTexture` 23,
  `GetRenderTargetData` 32, `StretchRect` 34, `CreateOffscreenPlainSurface` 36,
  `SetRenderTarget` 37, `BeginScene` 41, `EndScene` 42, `Clear` 43,
  `SetRenderState` 57, `SetTexture` 65, `SetTextureStageState` 67,
  `DrawPrimitiveUP` 83, `SetFVF` 89; `IDirect3DTexture9::GetSurfaceLevel` 18;
  `IDirect3DSurface9::LockRect` 13.
- **OLE picture probe** — `CreateStreamOnHGlobal` + `OleLoadPicture` +
  `IPicture::get_Handle` (vtable slot 3) + `BitBlt`/`StretchBlt`/`GetDIBits` into
  a 24bpp top-down DIB section, then report which rows are bright.

## Unpacking `booty.fs`

`gzip -dc booty.fs` → a `BOOFS1` archive whose name table is XOR `0xC8`
(`ambient.psh`, `overlay1.jpg` … `overlay15.jpg`, `cred1..3.jpg`, `loadbar.jpg`,
`*.psh`/`*.vsh` as precompiled `ps_3_0`/`vs_3_0` bytecode, `*.boo` scenes).
The images are stored as plain JPEGs and can be carved by walking markers from
each `FFD8 FFE0/FFE1`, skipping APP payloads so the EXIF thumbnail is not
mistaken for the image.

## Running the demo for tests

- demarc, headless, screenshots: `target/release-fast/demarc pcdemo/wrath/wrath.exe
  --headless --remote-control <lua>`. Gotchas: `wait_frames` counts emulator
  frames, which only start once gamescope and wine are up, so the **first**
  screenshot must be ~20 s in or later; screenshots closer than ~2 s apart hang
  the run; and a stale `/run/user/1000/demarc-wine-1000/<pid>` left by a killed
  run makes the next start hang — `rm -rf` it first.
- Plain wine, same launch demarc uses:
  `wine explorer /desktop=dbg,1280x720 system/win/demarc-autodlg.exe --launch
  pcdemo/wrath/wrath.exe --timeout 20 --prefer 16:9 --prefer 1280x720 --check Fullscreen`
- `-x wine_dialog_res=pick` leaves the setup dialog up, which is a handy
  known-orientation reference in a screenshot.
