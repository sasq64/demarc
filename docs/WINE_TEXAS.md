# keyboarders — Texas (nvision08 4k): why it does not run under wine

Working notes. Status: **the packed 4k still does not run; the official "safe"
build from the second release does.**

Files: `texas/kbd_Texas_720p.exe` (first release), `texas/rel2/` (second release,
adds more resolutions plus `rel2/safe/` — 94 KB unpacked builds that ship their
own `extrasafe.wma`).

## What the intro actually does

The 4k is Crinkler-packed, so nothing is visible in the file. Reconstructed from
a `WINEDEBUG=+relay` trace, and confirmed against the import table of the
unpacked `safe` build, which is the same program:

1. `LoadLibraryA` on `user32, d3d10, d3d9, d3dx10_33, d3dx9_33, ole32, shell32,
   winmm, wmvcore` (the safe build links `d3dx10_37` / `d3dx9_37` instead).
2. `D3D10CreateDeviceAndSwapChain` — the real renderer.
3. `Direct3DCreate9` + `CreateDevice` with an all-zero `D3DPRESENT_PARAMETERS`
   (fullscreen at the desktop mode). This device exists only so the **D3DX9 mesh
   helpers** work: `D3DXCreateBox`, `D3DXCreatePolygon`, `D3DXTessellateNPatches`,
   plus the `D3DXMatrix*` maths. Geometry is built with D3D9's D3DX and drawn
   with D3D10.
4. `D3DX10CreateEffectFromMemory` (profile `fx_4_0`) and `D3DX10CreateMesh`.
5. Audio: `SHGetSpecialFolderPathA(CSIDL_COMMON_MUSIC)` → `SetCurrentDirectoryA`
   → `SetCurrentDirectoryA("Sample Music")`, then one `IWMSyncReader`
   (`WMCreateSyncReader`) that pre-decodes **two Windows Vista sample tracks**:
   `One Step Beyond.wma` and `I Guess You're Right.wma`. The soundtrack is not in
   the 4k — it is whatever Vista shipped in `C:\Users\Public\Music\Sample Music`.

## The three blockers

### 1. wine's `d3dx10_33.dll` is a forwarder-only DLL, and Crinkler cannot follow forwarders

Original symptom:

```
wine: Unhandled page fault on read access to FFFFFFFF at address 796D7BAC
```

`796D0000` is where `d3dx10_33.dll` loaded, so the fault is at **RVA 0x7BAC**,
which `winedump -j export` gives as `D3DX10CreateEffectFromMemory`. Wine's
`d3dx10_33.dll` forwards **176 of its 177 exports** to `d3dx10_43`, so that RVA is
not code — it is inside `.edata` and holds the ASCII forwarder string:

```
007bac  64 33 64 78 31 30 5f 34 33 2e 44 33 44 58 31 30  >d3dx10_43.D3DX10<
007bbc  43 72 65 61 74 65 45 66 66 65 63 74 46 72 6f 6d  >CreateEffectFrom<
007bcc  4d 65 6d 6f 72 79 00                             >Memory.<
```

Crinkler does not use the PE import directory; its loader walks each DLL's export
address table itself and takes `base + EAT[i]`. `GetProcAddress` resolves
forwarders, a raw EAT read does not — so the intro jumps into the string and
executes `"d3dx10_43.D3DX10Cre…"` as x86. This will hit **any** Crinkler intro
against **any** all-forwarder wine DLL, not just this one.

wine's `d3dx9_33.dll` has 334 real exports and 0 forwarders, so the D3DX9 side is fine.

Fix: native Microsoft `d3dx10_33.dll` (+ `d3dcompiler_33.dll`, which it needs),
from `apr2007_d3dx10_33_x86.cab` inside the June 2010 DirectX redist.

### 2. The soundtrack is Vista's sample music, which no wine prefix has

```
err:wmvcore:reader_Open Failed to open L"One Step Beyond.wma", error 2.
```

then a NULL deref in the intro's own code at `0x004207D7`. The intro never checks
the `HRESULT`. Needs `One Step Beyond.wma` and `I Guess You're Right.wma` in
`drive_c/users/Public/Music/Sample Music/` (`SetCurrentDirectoryA("Sample Music")`
returning 0 is why the first attempts resolved to `…/Music/` instead).

### 3. wine's `IWMSyncReader::Open` refuses to re-open — this is the current blocker

With both tracks present, the intro decodes all of track 1, then calls `Open()`
again on the *same* reader for track 2 without a `Close()`:

```
trace:wmvcore:reader_Open reader 12E628D0, filename L"I Guess You're Right.wma".
warn:wmvcore:reader_Open Stream is already open; returning E_UNEXPECTED.
trace:wmvcore:reader_GetNextSample reader 12E628D0, stream_number 0, …
wine: Unhandled page fault on read access to 00000000 at address 00000000
```

Windows lets a sync reader be re-opened; wine's `reader_Open`
(`dlls/wmvcore/reader.c`) bails with `E_UNEXPECTED` if `wg_parser` is already set.
The intro ignores that, keeps calling `GetNextSample` on the exhausted first
stream, gets a zeroed sample back and calls through a NULL vtable slot — hence
`EIP = 0`.

No prefix-side workaround exists. The real fix is a small wine patch: have
`reader_Open` close the existing stream and reopen, as Windows does.

## What is currently installed in `~/.wine-demarc`

- `drive_c/windows/syswow64/d3dx10_33.dll` and `d3dcompiler_33.dll` — native,
  extracted from `~/.cache/winetricks/directx9/directx_Jun2010_redist.exe`
  (`cabextract` it, then `apr2007_d3dx10_33_x86.cab`).
- `HKCU\Software\Wine\DllOverrides`: `d3dx10_33 = native`, `d3dcompiler_33 = native`.
- `drive_c/users/Public/Music/Sample Music/*.wma` — the 11 Vista sample tracks,
  from `Sample Music For Vista.zip`.

To undo: delete those two DLLs and the two registry values.

## Repro

```sh
cd texas
WINEPREFIX=~/.wine-demarc wine kbd_Texas_720p.exe          # still dies, blocker 3
WINEPREFIX=~/.wine-demarc wine rel2/safe/texas_safe_1280x720.exe   # works
```

Useful traces: `WINEDEBUG=+loaddll` (which DLL faulted), `+relay` (Crinkler's
`LoadLibraryA` list and the API sequence — note DXVK's `d3d9`/`dxgi`/`d3d10core`
are native so they are *not* relayed), `+wmvcore` (blocker 3), `+file` (blocker 2).
`winedbg` does not produce a backtrace in this prefix — it fails to start the
process — so everything above came from traces.

## Next steps

1. **Use the safe build.** `rel2/safe/texas_safe_1280x720.exe` runs to completion.
   It has a normal import table (so no Crinkler forwarder problem), ships its own
   `extrasafe.wma` (so no Vista sample music), and only logs one harmless
   `fixme:d3dx:D3DXTessellateNPatches … stub`. An `overrides.toml` entry keyed on
   the demozoo id should point at it and pick the resolution variant.
2. Consider extending `NATIVE_DLLS` in `src/newsys/windows.rs` from `d3dx9*.dll`
   to cover `d3dx10*.dll` — same argument, and blocker 1 makes wine's d3dx10
   actively dangerous for Crinkler releases.
3. Optional: patch wine's `reader_Open` to allow re-open, and send it upstream.
   That is what blocker 3 needs, and it is likely to affect other Vista-era
   productions that stream more than one file through one sync reader.
