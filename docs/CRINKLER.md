# Crinkler imports under wine

[Crinkler] packs a 4k intro into a few kilobytes by, among other things, throwing away the
PE import table. Nothing in the file says `kernel32.dll!CreateSemaphoreA`; instead the
depacker walks a loaded module's export directory at startup, hashes each name, and keeps
the ones whose hash it was told to look for. That works because the export table on a real
Windows is what the intro was linked against.

wine's export tables are *not* that table. They hold roughly the same names in roughly the
same order, and "roughly" is where these prods break. Two failure modes have shown up so
far. Both are silent — the intro runs, the window opens, nothing crashes — so neither is
findable without a relay trace.

---

## Mode 1 — range imports land on the wrong export

Crinkler's range import finds one export by hash and takes the ones after it *by position*
in the export address table. Any name wine is missing shifts everything behind it by one.

Alcatraz' "Prism break" calls `waveOutWrite` and ends up in winmm's export *name* table,
because wine's winmm has no `tid32Message` and the other `*32Message` exports.

Fixed by `tools/winmm`: a 32-bit `winmm.dll` whose ordinals match the Windows one, each
export jumping into wine's builtin winmm. `just winmm` builds it, `scripts/setup-wine.sh`
installs it, demarc sets `winmm=n,b`. See `tools/winmm/README.md`.

---

## Mode 2 — a missing name falls back to export #1

When the hash search finds *nothing*, the lookup does not fail loudly; it comes back with
index 0, i.e. **ordinal 1, the alphabetically first export**. For `kernelbase.dll` that is
`AccessCheck`.

This is worse than a wrong return value, because the fallback has its own `stdcall`
argument count. `AccessCheck` takes **8** parameters. An intro that pushed **4** and
expected them cleaned up gets 32 bytes popped instead of 16, and its `esp` is left 16 bytes
high for the rest of the function.

### The case: RGBA — ixaleno (Breakpoint 2008)

`CreateSemaphoreA` is the name wine's `kernelbase.dll` does not export — it has only
`CreateSemaphoreW` and `CreateSemaphoreExW`. (wine's `kernel32.dll` *does* export it, at
its own RVA. The intro never looks there; see "Which module" below.)

```
00dc:Call kernelbase.AccessCheck(00000000,00000010,00000010,00000000,
                                 000002d0,00000500,00000000,7bf55f38) ret=0042018e
```

Arguments 1–4 are the intro's `CreateSemaphoreA(NULL, 16, 16, NULL)`. Arguments 5–8 are
stack it ate on the way past — note `0x2d0`/`0x500`, the intro's own 720 and 1280.

The caller is the "spawn 16 render threads and wait" function. Its epilogue then reads its
own locals instead of the registers it saved:

```asm
0x42021d: pop edi   ; [ebp-0x110] = thread0.x_end    -> 0x40
0x42021e: pop esi   ; [ebp-0x10c] = thread0.semaphore -> 0
0x42021f: pop ebx   ; [ebp-0x108] = thread1.x_start  -> 0x40
0x420220: leave     ; esp restored from ebp -> the caller sees nothing wrong
```

`leave` is why there is no crash and no bad frame: the damage is confined to three
registers, and the display loop is what reads them.

```asm
0x420127: push $0xcc0020 / push ebx / push $0x421a88 / push ebp
          push edi / push esi / push ebx / push ebx      ; src  h,w,y,x
          push edi / push esi / push ebx / push ebx      ; dest h,w,y,x
          push 0x44(%esp)                                ; hdc
0x42013f: call *0x4300d0                                 ; StretchDIBits -> 0, draws nothing
0x420145: push $5      / call *0x43004c                  ; Sleep(5)
0x42014d: push $0x1b   / call *0x4300bc                  ; GetAsyncKeyState(VK_ESCAPE)
0x420158: je 0x420127
```

`DestWidth` is `esi`, which is now 0, so every blit is a no-op and the intro spins there
forever. The one blit that *does* succeed is the one issued before rendering starts, on an
empty buffer — that is the black screen.

**The raytracer itself is fine.** Dumping the DIB out of process memory once it settles
gives the correct picture, identical to the prod's own `final.png`.

### Second symptom, same cause

With `esp` 16 bytes high the *spawn loop* also writes high. Its first `push` — the
`lpThreadId` out-param — lands on `ebp-0x114`, which is thread 0's `x_start`. That field
becomes a stack address, thread 0 hits its `cmp edi,ebx / jge done` guard and exits without
drawing. So once the blit is fixed the leftmost `width/16` columns are still empty.

All three resolutions are affected identically; the corrupted width is always `width/16`
(`0x40` at 1024, `0x50` at 1280, `0x78` at 1920), which is a quick way to recognise it.

---

## Mode 3 — forwarded exports

Crinkler reads `base + EAT[i]` without following forwarders, and 94 of wine's
kernelbase exports are forwarders to ntdll. The intro jumps into the forwarder
string and runs it as code. Unlike modes 1 and 2 this crashes, usually with
`EXCEPTION_PRIV_INSTRUCTION` (`n` is `outsb`) at an address inside kernelbase's
`.edata`. quite's "yes we can" hits it on `QueryPerformanceFrequency`.

Fixed by `tools/kernelbase`, the same idea as `tools/winmm`.

---

## Which module Crinkler reads

Every slot in ixaleno's IAT resolves to a **`kernelbase.dll`** address — `CloseHandle`,
`CreateThread`, `GetSystemInfo`, `Sleep`, `WaitForMultipleObjects`, `WaitForSingleObject`,
`ExitProcess`. None resolve into `kernel32.dll`, and the lookup does not fall through to
kernel32 when a name is absent from kernelbase.

*Why* it settles on kernelbase rather than kernel32 was not established — that needs the
depacker's own resolver disassembled, and it is overwritten by the time the intro is
running. It matters only if a fix wants to move the lookup rather than satisfy it.

---

## Diagnosing one of these

A Crinkler prod that runs, shows nothing, and does not crash is the signature.

```sh
# 1. what is it actually calling? a spinning blit loop with a zero dimension is mode 2
WINEDEBUG=+relay timeout 25 wine prod.exe 2>&1 | grep -E "Call (gdi32|user32)\."

# 2. the tell: a call to AccessCheck whose return address is inside the prod itself
WINEDEBUG=+relay timeout 25 wine prod.exe 2>&1 | grep AccessCheck
```

An `AccessCheck` with a `ret=` inside the prod's own code is always this bug; the return
address names the call site. To find which import it should have been, attach and dump the
IAT — unresolved slots all share one address, and it is export #1 of the module:

```sh
setsid wine prod.exe & sleep 5
PID=$(printf 'info process\nquit\n' | wine winedbg 2>&1 | grep -i prod | awk '{print $1}')
printf "attach 0x$PID\nx/56x 0x00430000\ndisas 0x00420090,0x00420230\nquit\n" | wine winedbg
```

(Addresses are ixaleno's. Crinkler unpacks to a fixed layout, so a different prod needs its
own entry point found first; `disas` reads the *decompressed* code, which is the only way to
see it.) The same trick dumps the framebuffer — `x/65536x <bits>` in chunks, using the
`lpBits` and `lpbmi` from the first successful `StretchDIBits` — which is how you confirm
the renderer is innocent before chasing it.

---

## Fixing mode 2

The intro does not need the semaphore. Its workers call `WaitForSingleObject(h, 0)` and
ignore the result, so a NULL handle is harmless; only the argument-count mismatch does
damage. Exporting `CreateSemaphoreA` from wine's kernelbase — forwarding to
`CreateSemaphoreW`, which is correct whenever `lpName` is NULL — fixes both symptoms.

Options, in order of how much they touch:

1. **Patch wine's kernelbase spec.** One line, but it means shipping a wine build.
2. **A shim**, the way `tools/winmm` does it. More work than winmm's, because kernelbase is
   not something a prefix can override as freely as winmm.
3. **Nothing.** The prod stays black. Worth knowing that no amount of core-option or
   prefix tuning will move it — this is below all of that.

Neither 1 nor 2 is implemented. Note that any fix for this class has to preserve the
*argument count*, not just the name: a stub returning 0 with the wrong `ret N` reproduces
the bug exactly.

[Crinkler]: https://www.crinkler.net
