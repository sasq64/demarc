# Amiberry libretro core — status notes

Investigation into using Amiberry's libretro core as an alternative Amiga
backend to the current `puae` core. Written 2026-08-25.

Checkout: `external/amiberry` @ `552274b6` (upstream `BlitterStudio/amiberry`,
branch `master`). The `libretro/` port is maintained upstream, not by us.

**Bottom line:** the core builds, boots, and runs demos, **including TBL's
Starstruck** (the definitive test), which now loads in ~11 s and plays.

The blocker recorded here earlier — "the directory-filesystem read path is
~1000x slower than puae's" — was a misdiagnosis. The filesystem was never the
problem; the emulated CPU was throttled to the modelled machine's clock. See
[Starstruck](#starstruck-tbl--the-definitive-test).

---

## Building

```sh
cd external/amiberry/libretro
git -C .. submodule update --init libretro/libco libretro/libretro-common
make JIT=1 -j24          # -> amiberry_libretro.so (~32 MB)
```

Two gotchas:

- `libretro/libco` and `libretro/libretro-common` are **submodules**. Without
  them the build dies on `libco/libco.h: No such file or directory`.
- The Makefile has **no dependency tracking on `FLAGS`**, so toggling `JIT=1`
  does not rebuild anything. `make clean` first, always. Skipping it produces a
  confusing `multiple definition of check_prefs_changed_comp` link error: the
  stale `newcpu.o` still carries the non-JIT stub from `src/newcpu.cpp:71`,
  which lives in the `#else` of `#ifdef JIT`.
- `deps/nlohmann/json.hpp` is fetched by the Makefile on first build (network).

## Testing it in demarc

`$DEMARC_CORE_DIR` (re-added to `src/libloader.rs`) is a colon-separated list of
directories searched for `<name>_libretro.<ext>` before the buildbot cache. A
hit short-circuits the whole download/cache path, so a local build is never
written to — or evicted from — the user's cache.

demarc asks for the core by name, and for Amiga that name is still `puae`, so
the local build has to be presented under that name:

```sh
mkdir -p /tmp/cores
ln -sf $PWD/external/amiberry/libretro/amiberry_libretro.so /tmp/cores/puae_libretro.so
DEMARC_CORE_DIR=/tmp/cores ./target/release-fast/demarc -w tbl/
```

Confirm which library actually got loaded with
`grep libretro.so /proc/$(pgrep -x demarc)/maps` — demarc copies the core to
`/tmp/demarc-core-XXXXXX/` first, and the file size distinguishes the two
(amiberry ~32 MB, puae ~21 MB).

---

## Our changes

`libretro/Makefile` (+3), `libretro/libretro.cpp` (+140). Every new option
defaults to the old behaviour, so upstream defaults are untouched. Nothing
under `src/` is patched — the fixes all live in the libretro port.

| Change | Where |
| --- | --- |
| `JIT=1` opt-in guard around the `-DLIBRETRO_NO_JIT` block | `Makefile:431` |
| `amiberry_jit` option — sets `cachesize` + clears cycle-exact | `libretro.cpp:684`, `:814`, `:4101` |
| `amiberry_cpu_speed` option (`default`/`real`/`max`) | `libretro.cpp:686`, `:830`, `:4132` |
| `amiberry_z3mem_size` option (up to 512 MB) | `libretro.cpp:683`, `:790` |
| `amiberry_chipmem_size` / `amiberry_bogomem_size` / `amiberry_fastmem_size` options (puae value space) | `libretro.cpp` variables + option_defs, chip/bogo/fastmem push |
| `amiberry_cpu_model` extended to 68040/68060 (+ matching FPU, `address_space_24=false`) | `libretro.cpp` cpu_model push |
| Directory-as-harddrive mount | `libretro.cpp:4281` |
| A4000 kickstart fallback to `kick40068.A1200`, now with a warning | `libretro.cpp:1826`, `:1890` |
| Floppy images pushed as `-s floppy0=` instead of a positional arg | `libretro.cpp:1868`, `:5089` |

### Why the floppy path can't be a positional argument

`main.cpp`'s positional dispatch matches the disk-image extensions with the
case-**sensitive** `_tcscmp` (`main.cpp:1542`), and `get_filename_extension`
(`main.cpp:1248`) returns the extension verbatim — no case folding. The `.rp9`
and `.lha` arms one screen up use `_tcsicmp`, so the inconsistency is plainly a
slip rather than a decision.

An uppercase `.DMS` or `.ADF` therefore misses every arm, falls through to the
generic tail that only recognises configs and statefiles, and is dropped in
silence; the core logs *"No game content provided; booting to Workbench"* and
you get the Kickstart insert-disk hand. Scene releases are full of uppercase
extensions (`PHENOMENA-Enigma.DMS` is what found this), so it is not an edge
case. `.fdi` and `.raw` are advertised in `valid_extensions` but absent from
that branch in any case, so they never inserted at all.

`-s floppy0=<path>` sets `floppyslots[0].df` (`cfgfile.cpp:6673`) — the same
field `disk_insert(0, ...)` would have written, and the one the disc-swap code
reads back (`libretro.cpp:2437`) — while bypassing the extension matching
entirely. This mirrors what the CD branch already does with `cdimage0=`
(`libretro.cpp:5039`), for a related reason.

`.uae` deliberately keeps the positional path: a config file handed over as
content still has to reach main.cpp's config loader. The one thing given up is
main.cpp's lookup of a `<image name>.uae` in the configurations dir, which
would fight with the core options anyway.

The case-sensitivity itself is worth fixing upstream (`_tcscmp` → `_tcsicmp` on
`main.cpp:1516`, `:1534`, `:1542` — `.uss` and the CD extensions have it too),
but that is `src/`, which we do not patch.

On the demarc side: `$DEMARC_CORE_DIR` in `src/libloader.rs` (`local_core()`),
`cap_malloc_arenas()` in `src/main.rs` (see [the JIT cache
section](#rendering-speed-the-jit-cache-was-8-kb)), and `tbl/demo.m3u` now
carries the full A4040 config.

### Why JIT needed more than removing `-DLIBRETRO_NO_JIT`

Worth writing down, because it is not obvious and it affects **every platform,
arm64/macOS included** (where `LIBRETRO_NO_JIT` is never defined and JIT is
compiled in already):

1. The model presets do set the JIT cache — `p->cachesize = MAX_JIT_CACHE`
   under `#ifdef JIT` (`cfgfile.cpp:9448`, `:9506`, `:9573`). `libretro.cpp`
   itself never sets it, and does not need to.
2. But `main.cpp`'s `--model` handler calls the **2-arg Amiberry wrappers**
   (`cfgfile.cpp:10810`), which call `bip_xxx(p, 0, 0, 0)` directly and so skip
   the `buildin_default_prefs(p)` that `built_in_prefs()` runs first
   (`cfgfile.cpp:10215`). compa is also hardcoded to 0, so the compa==4 branch
   that sets cachesize is unreachable.
3. That leaves `cpu_cycle_exact` at whatever `default_prefs` set, and
   `fixup_cpu` then zeroes cachesize with *"JIT and cycle-exact can't be
   enabled simultaneously"* (`main.cpp:361`).

So a preset's own cachesize never survives. Our `amiberry_jit` option works
around it by pushing `cpu_cycle_exact=false` / `cachesize=16384` as `-s` args
*after* `--model`, where they are applied late enough to win.

**The cleaner fix is upstream**: route the 2-arg wrappers through
`built_in_prefs()` so presets behave as designed. That touches the desktop GUI
too, so we did not attempt it.

Verified working: `JIT: cache=16384. b=0 w=0 l=0` (`b/w/l=0` is comptrust
direct — the fast path, not the indirect fallback) and
`CPU=68040, FPU=68040, MMU=0, JIT=CPU/FPU=16384`.

JIT is off by default because it forces cycle-exact off, which is wrong for
timing-sensitive OCS/ECS software.

---

## Integration gaps vs. the puae core

**1. Option namespace.** demarc emits `puae_*` (`src/newsys/amiga.rs:21`);
Amiberry reads `amiberry_*` (`libretro.cpp:677`). Every option is silently
dropped, so everything runs as a default OCS A500 / 68000 / KS 1.3 — AGA
included. Proven on rebels.adf:

| | `puae_*` (what demarc sends) | `-x amiberry_model=A1200,amiberry_cpu_model=68030` |
| --- | --- | --- |
| Chipset mask | `00000000` (OCS) | `00000007` (AGA) |
| Kickstart | KS 34 (1.3) | KS 40 (3.1) |
| CPU | 68000 | 68030 |

A mapping layer in `amiga.rs` is the obvious fix, but note it cannot be a pure
rename — see the option-coverage gaps below.

**2. Option coverage.** Even renamed, Amiberry's libretro exposes no equivalent
for `puae_fpu_model`. We added `amiberry_z3mem_size`, `amiberry_chipmem_size`,
`amiberry_bogomem_size` and `amiberry_fastmem_size` (all four taking the same
values as their `puae_*` counterparts, so they are plain renames in
`amiga.rs`), `amiberry_cpu_speed` (the nearest thing to `puae_cpu_throttle`,
and the one that matters — see Starstruck below) and 040/060 CPUs; the rest are
still missing.

**3. Startup ROM scan cost.** Amiberry recursively CRC32/SHA1-scans the whole
demarc `system/` dir (fuse, vice, musix, pcem ROMs, win…) on every load — about
2 s added per demo start.

**4. Geometry differs.** Amiberry reports base 640x480 / max 1920x1280 and
`GEOMETRY 720x568`; puae reports 360x287 / 720x574 and `GEOMETRY 696x264`. So
demarc's crop/aspect defaults are mistuned for it.

---

## Starstruck (`tbl/`) — the definitive test

Requirements per its readme: AGA, 68060 @50MHz+, ~50 MB fastram, 2 MB chipram.

`tbl/demo.m3u` was added to make this loadable. Two reasons, both about how
demarc collects files, not about Amiberry:

- A directory of **mixed** file types is split into one entry per file
  (`src/files.rs:340`), so `demarc tbl/` failed with *"No system recognized for:
  Starstruck-Final.txt"*. An m3u with no file entries makes the directory a
  single unit and carries config as metadata — the convention `motocross/`
  already uses.
- It also flips `copy_all` (`src/newsys/amiga.rs:370`). Passing the `.exe`
  directly copies **only** the exe as `amiga_file` and leaves the 18.7 MB
  `Starstruck-Final.dat` behind, so the demo cannot load its data.

### Config that works

```
amiberry_model="A4040" amiberry_cpu_model="68040"
amiberry_z3mem_size="64" amiberry_jit="enabled"
amiberry_cpu_speed="max" amiberry_kickstart="kick40068.A4000"
```

That is what `tbl/demo.m3u` carries. `amiberry_cpu_speed="max"` is the one that
made it load at all (see below); on A4040 it and `amiberry_jit` are belt and
braces, since `bip_a4000()` sets `m68k_speed = -1` and `cachesize` itself
instead of going through `set_680x0_compa()`.

One real dead end: **68060 fails** with `FPU UNIMPLEMENTED INSTRUCTION/FPU
DISABLED EXCEPTION`. 68040 is clean.

### A4000 needs an A4000 Kickstart

The note here used to say A4000/A4040 died with `Out of memory` because
Amiberry's autoconfig enumeration stopped before the Z3 board. That was wrong.
It was the Kickstart: no `kick40068.A4000` was present, so
`find_kickstart_in_system_dir()`'s last-resort fallback handed the A4000 config
the *A1200* ROM. Both ROMs are 3.1 rev 40.68, but they are not
interchangeable — with the A1200 ROM the machine comes up with Gayle and an
A1200 battery clock, and AmigaOS adds neither the RAMSEY motherboard RAM nor
the Zorro III board. Starstruck then prints, on screen:

```
Error: 5: AmigaOSPool_allocate 50331676 bytes from A...
Error: 8: sysCreateStandardPool
Error: 9: Out of memory
```

50331648 = the 48 MB block. With `system/kick40068.A4000` in place the same
A4040 config maps `RAMSEY memory (low) 8M` at `0x07800000` and `Zorro III Fast
RAM 64M` at `0x40000000`, AmigaOS takes both, and the demo loads and runs.

The fallback is still there so an A4000 config boots at all without the right
ROM, but it now logs a warning saying the machine has no usable fast RAM.

### The blocker, and why it was not the filesystem

The demo reached its AGA loading screen and crawled. Measured with a temporary
profiler in `filesys_iteration()` (`src/filesys.cpp`) logging, per DOS packet,
how long the filesystem thread had been *blocked* waiting for the next packet,
how long its own *work* took, and how many emulated frames had passed:

The demo's main data load is a run of 64 KB `ACTION_READ`s, so those are the
interesting packets:

| 64 KB reads | rate | throughput | host work per read | emulated frames between reads |
| --- | --- | --- | --- | --- |
| `cpu_speed` unset | 0.015–6 /s | 1–30 KB/s | 20–70 us | 351 and up |
| `cpu_speed=max` | ~28 /s | **~1.9 MB/s** | 20–70 us | 1–2 |

The host side was never slow: a 64 KB read costs tens of microseconds either
way, and the reply interrupt reaches `exter_int_helper` 15–30 us after the
filesystem thread signals it. What changed is the last column — the *guest* was
spending 7 to 65 seconds of emulated time between one 64 KB read and the next,
and the emulator was faithfully running those seconds at a true 50 fps while
using ~16% of one host core.

The cause is the CPU speed. `buildin_default_prefs_68020()` does set
`m68k_speed = -1` (unthrottled), but `bip_a1200()` then calls
`set_68020_compa(p, compa, 0)` (`cfgfile.cpp:9883`), and `compa == 0` — which is
what the 2-arg `bip_*` wrappers `main.cpp` uses always pass — puts it back to
`m68k_speed = 0`, "real" (`cfgfile.cpp:9409`). The emulated CPU is throttled to
the modelled machine's clock even when it is a 68040, so Starstruck's depacker
runs at roughly A1200 speed. p-uae defaults to unthrottled, which is why this
looked like a puae-vs-Amiberry filesystem gap.

Note this is the *same* root cause as the JIT problem below: compa is hardcoded
to 0, and compa 0 means both "cycle-exact" and "real speed".

`amiberry_cpu_speed="max"` pushes `cpu_speed=max` as a `-s` argument after
`--model`, the same trick `amiberry_jit` uses, so it is applied late enough to
win over the preset.

### Numbers

Reading all 18,755,671 bytes of `Starstruck-Final.dat`, sampled from
`/proc/<pid>/fdinfo` (the host fd advances in 64 KB stdio refills, so it tracks
guest progress closely):

| | time to read the file | rate |
| --- | --- | --- |
| Amiberry, `cpu_speed` unset | never finished — 128 KB read, then 1–30 KB/s | — |
| puae (demarc's normal core) | ~78 s | ~240 KB/s |
| Amiberry, `cpu_speed=max` | **~11 s** | **~1.8 MB/s** |

Sampled with a shell loop over `/proc/<pid>/fd` looking for the open
`Starstruck-Final.dat`; puae's own run is the honest baseline, and the earlier
note here that puae read the file "well within 10 s" was wrong too.

After that the demo plays — TBL logo, then the scene.

### Rendering speed: the JIT cache was 8 KB

Loading was fixed but the demo still rendered sluggishly. With
`RUST_LOG=retro=debug` the core's own log says why:

```
JIT: WARNING: could not allocate within 2GB of globals (anchor=0x7f74398b0000)   x11
JIT: <JIT compiler> : actual translation cache size : 8 KB at 0x7f746c000000-...
```

The x86-64 JIT addresses the emulator's globals RIP-relative, so its
translation cache has to sit within ±2 GB of them. `alloc_cache()` halves the
request whenever `alloc_code()` fails, and here it halved eleven times: the
16 MB cache became **8 KB**, which thrashes.

There was genuinely nowhere to put it. Dumping `/proc/<pid>/maps` in the window
around the core's `.bss`:

- **below**: Amiberry reserves 4 GB of "natmem" ending exactly at the core
  library's base, so the whole lower 1.75 GB is natmem;
- **above**: demarc's own mappings, and glibc reserves **64 MB of address space
  per malloc arena** while allowing `8 * nproc` of them. On this 24-core box
  that is up to 192 arenas.

Largest free gap in the whole ±1.75 GB window: 12 KB.

`mallopt(M_ARENA_MAX, 8)` as the first thing `main()` does
(`cap_malloc_arenas()`, `src/main.rs`) gives the window back:

| | JIT translation cache | failed allocations |
| --- | --- | --- |
| default | 8 KB | 11 |
| `M_ARENA_MAX` 16 / 8 / 4 | 16384 KB | 0 |

Confirmed by eye as well — "visibly smoother rendering". This is demarc's
problem rather than Amiberry's (standalone Amiberry has an empty address space)
and it applies to **any** JIT core, p-uae included.

### Two things this rules out

- **`do_uae_int_requested()` does not wake the CPU.** It only sets
  `uae_int_requested` (`native2amiga.h:53`); the request becomes a real IRQ2
  only when the guest next writes INTREQ (`custom.cpp:3319` → `rethink_intreq`
  → `devices_rethink` → `rethink_uae_int`), which while a program waits on a
  packet is the VERTB ack. That does quantise filesystem replies to a few per
  frame. Adding `set_special_exter(SPCFLAG_UAEINT)` there (the way
  `hwtrap_check_int()` does) was tried and measured: reply latency is 15–30 us
  either way and throughput is identical (1.90 vs 1.89 MB/s), so the change was
  reverted. Worth remembering if a workload ever does turn out to be
  reply-latency-bound.
- **A hardfile image would not have helped.** The directory filesystem was
  never the bottleneck, and an HDF adds the ROM FFS on top of the same guest
  CPU. Not tried, and no longer a reason to.

## Open items

1. **`puae_*` → `amiberry_*` mapping** in `amiga.rs`, plus the missing options
   (fpu_model). Without it every demo still runs as
   a default OCS A500 unless its m3u spells out `amiberry_*` by hand, the way
   `tbl/demo.m3u` does.
2. **Upstream `bip_*` wrapper fix** — route the 2-arg wrappers through
   `built_in_prefs()` so JIT and CPU speed (compa generally) work without our
   `-s` workarounds. This is the root cause behind both `amiberry_jit` and
   `amiberry_cpu_speed`, and fixing it upstream would retire both.
3. **Whether `cpu_speed=max` should be the default** for accelerated models.
   It is opt-in now because unthrottling is wrong for timing-sensitive OCS/ECS
   software, but a 68040/68060 config almost always wants it.
4. Startup ROM scan cost, and geometry/crop defaults.

### Done since the last pass

- Directory-filesystem read throughput — was not a filesystem problem; see
  [Starstruck](#starstruck-tbl--the-definitive-test).
- A4000 Z3 autoconfig — not an autoconfig bug at all; it needed an A4000
  Kickstart. `system/kick40068.A4000` is now present and `tbl/demo.m3u` runs
  the demo as an A4000/040.
- Rendering speed — the JIT was running on an 8 KB translation cache; capping
  glibc's malloc arenas gets the full 16 MB.
- Local-core override — `$DEMARC_CORE_DIR` is back in `libloader.rs`
  (`local_core()`, with a test), so testing no longer means overwriting the
  core cache.
