
## NEW PROCESS

`collect_files() -> Vec<EmuFile>` as usual

but ask systems for leaf dirs without m3u to know if whole dir
or individual files should be added.


### Loading FileSource

* If source is list of URLs, download all to temp dir

* `


### Loading EmuFile

non system prepare:

Download
Unpack archives


Pass EmuFile to all Systems until matched

Always single file or directory

"GAME" INFO

Meta data from disk or db, handled by frontend.
GameInfo for database / display


title
author
date
party
platform
category
tags
download


## HANDLING IMAGES

Incoming is DIR or SINGLE IMAGE
DIR can be temp or not

Collect images

Write m3u where?






can load -> WorkFile

can create Backend

`Load <WorkFile>`

- Supported file: Direct load
- Directory
  * Maybe: convert files 
  - First supported file
  - Collect disk images -> m3u
  - Load directory

Iterate dir -> supported files.
Disk images -> m3u, else first supported
  new

## RULES

Normal: Directory -> load()
Usually tranforms to temp dir. All files copied

File -> load(): Only that file loaded. If temp-dir required can mess up.

 
FLASH SPEED

```
┌────────────────────┬───────┬───────────────────────────────────────────────────────────┐
│       stage        │ cost  │                        what it is                         │
├────────────────────┼───────┼───────────────────────────────────────────────────────────┤
│ tick (AVM          │ ~34   │ Away3D doing software 3D in ActionScript, run by Ruffle's │
│ run_frame)         │ ms    │  interpreter                                              │
├────────────────────┼───────┼───────────────────────────────────────────────────────────┤
│ render             │ ~2 ms │ wgpu drawing the display list                             │
├────────────────────┼───────┼───────────────────────────────────────────────────────────┤
│ capture_frame      │ ~7 ms │ GPU readback (your suspicion)                             │
└────────────────────┴───────┴───────────────────────────────────────────────────────────┘
```

NAGA FIX


Where it breaks

glslang/shaderc compile GLSL modf(x, out ip) to a SPIR-V GLSLstd450 ModfStruct (returns a {fract, whole} struct, then an OpStore for the pointer). naga's SPIR-V frontend maps that here:

- src/front/spv/next_block.rs:1740 — Glo::ModfStruct => Mf::Modf (and :1748 FrexpStruct => Frexp)
- :1810 — it appends Expression::Math { fun: Modf, .. } and moves on.

The problem: Math{Modf}'s result type is a predeclared special type (ModfResult). The typifier resolves it by looking that type up in module.special_types.predeclared_types (src/proc/typifier.rs → src/proc/overloads/rule.rs:96), and if it's absent returns MissingSpecialType — exactly our validation error. The frontend created the expression but never registered the type.

Contrast the atomic path in the same file (next_block.rs:3003), which does it correctly:
let atomic_result_struct_ty_h = ctx.module.generate_predeclared_type(
    crate::PredeclaredType::AtomicCompareExchangeWeakResult(scalar),
);
The Modf/Frexp arms simply omit the analogous call.

What the upstream naga patch looks like

In the ModfStruct/FrexpStruct arms, after resolving the argument's scalar/vector size, register the type (the call is idempotent — it early-returns if already present):

Glo::ModfStruct => {
    // resolve arg's type → (size: Option<VectorSize>, scalar: Scalar)
    ctx.module.generate_predeclared_type(
        crate::PredeclaredType::ModfResult { size, scalar },
    );
    Mf::Modf
}

That's sufficient because:
- generate_predeclared_type builds ModfResult as { fract @0, whole @1 } (src/front/type_gen.rs:527), and SPIR-V's ModfStruct uses the same member order — so the downstream OpCompositeExtract 0/1 line up with no remapping.
- naga computes the Math expr's type from the typifier, not from the SPIR-V result_type_id, so registering the predeclared type is all the validator needs.

Frexp is identical with FrexpResult { size, scalar }.

Note there's a second, separate gap at next_block.rs:1770: the pointer-form Glo::Modf | Glo::Frexp is flatly UnsupportedExtInst (the TODO: gfx-rs/naga#2526 referenced there). But glslang emits the Struct form for shader modf, which is why we got MissingSpecialType (expression built) rather than UnsupportedExtInst. So fixing the struct arms resolves the real-world cases; full pointer-form support is a larger, orthogonal change.

CRT-ROYALE RED VERTICAL CENTER LINE

Root cause: crt-royale tiles its phosphor mask at a fixed triad size (default 3 px → 24 px tiles). When render_width / tile_size is an even integer (e.g. 2880/24 = 120), a tile boundary lands exactly on the center column, where the mask's manual frac() tiling has a coordinate discontinuity that duplicates a red subpixel. Only shows at even-divisor resolutions.

crt-royale's own fix (FIX_DISCONTINUITIES) uses ddx/ddy in a header that's also compiled for the vertex stage, where derivatives are illegal on the slang/glslang path — fails to compile (phosphor-mask-resizing.h:653 'dFdx'). It's off by default in stock crt-royale too, so RetroArch has the same tiling and only looks clean because it isn't at an even-divisor size.

Fix (src/post_process.rs): each frame, for the active CRT chain, pick the integer tile size in 22–26 px whose screen center sits furthest from a tile boundary and set mask_triad_size_desired at runtime via librashader. Keeps triads ~3 px (visually identical, mask stays pixel-sharp) and pushes the seam off-center; picks 24 (the stock default) when the center is already clear, so it only acts at pathological resolutions.

Caveat: this removes the prominent center line but doesn't eliminate crt-royale's underlying per-tile frac() discontinuity — a faint per-tile seam is inherent and only the (uncompilable) derivative fix would remove it fully.

ATARI ST DIRECTORIES AS GEMDOS HARD DRIVES

hatari mounts a host directory as GEMDOS drive C:, which is how a release
directory (program + data files) is loaded — the equivalent of handing puae an
Amiga directory. Two pieces of libretro-core trivia make it work:

- The core recognizes a hard drive by the *extension* `.gem`, and mounts the
  path with those four characters chopped off (`libretro/libretro.c`,
  `retro_load_game`). So `prepare_file` stages the drive as `<temp>/harddrive`
  and hands the core an empty `<temp>/harddrive.gem` next to it.
- On that path the core also inserts `<system>/hatari/BOOT.ST` into drive A: and
  passes `--disk-a` *before* `--harddrive`. hatari's option parser stops at the
  first bad argument, so a missing BOOT.ST means `--harddrive` is never parsed
  and there is no C: at all. `system/hatari/BOOT.ST` is therefore a blank 720K
  FAT12 floppy that only has to exist (`mkfs.fat -F 12 -S 512 -s 2 -r 112 -f 2
  -M 0xF9 -R 1 -h 0 -n BOOT --invariant -C BOOT.ST 720`). Nothing boots from it:
  `--harddrive` comes last and sets `bBootFromHardDisk`, so TOS boots from C:.

Booting from C: is what runs `C:\AUTO\*.PRG`, so the program is copied there as
`STARTME.PRG` (TOS only auto-starts `.PRG`). A directory holding nothing but the
executable keeps taking the old route — wrapped in a bootable floppy image —
since that is how ST demos were released and shipped.

Which program, and what to do about the release's own `AUTO` folder, was learned
from tlk2_hd.zip (TalkTalk 2, HD version):

- Its `AUTO` holds `LOADER.PRG` and `DISK2/3/4.PRG` — the disk-swap stubs of the
  floppy version. Deferring to an existing `AUTO` folder meant booting straight
  into "This is TalkTalk2 disk 2. Please insert disk 1 and reboot." So an `AUTO`
  folder is only trusted when the program we start is *in* it; otherwise it is
  renamed `NOAUTO` in our copy. The release's own README agrees: "consider
  removing all accessories and AUTO programs to free up memory".
- Its main program `TLKTLK2.PRG` (124K) sits next to `TLK2READ.PRG` (40K), a
  readme viewer, so picking the alphabetically first program picked the readme.
  The biggest program of the shallowest level wins instead; `boot_file` overrides
  it outright.

Size alone then broke on molz (More Or Less Zero, DHS), where it started the
demo but the music was garbage:

- The release is a 651-byte `molz.tos` loader next to `part1.bin` (652K) and
  `part2.bin` (201K). The parts are GEMDOS executables too, so the biggest one
  won and `part1.bin` was started directly. The loader is what reads `part1.mus`
  (604K of STE DMA samples) and passes it to the part — skip it and the part
  runs a replayer on whatever memory it was handed.
- Extension is therefore ranked before size: a file named the way TOS starts one
  (`.prg`, `.tos`, `.ttp`, `.app`) beats an executable named anything else,
  however big. A release names what the user runs and leaves its payload looking
  like data.
- It needs a 4MB STE and quietly returns to the desktop on the 1MB ST default,
  which is why the hard drive path defaults to `hatari_machinetype=ste` and
  `hatari_ramsize=4`.

If a demo ever turns out to dislike being started from `AUTO` (it runs before the
desktop), the faithful alternative is what hatari does internally, in `tos.c`:
write the boot drive's INF file — `NEWDESK.INF` for TOS >= 2.00, `DESKTOP.INF`
below — with a `#Z 01 C:\PROG.PRG@` line, and the desktop launches it from its
own directory.



# FADE / CROSS-FADE

Emu 0 main, Emu 1 next

load into 1, wait done
fade 0 and 1
OR
scroll 0 and 1
...
crossfade-shader ?








export allow_glsl_120_subset_in_110=true

Why the old fix wasn't safe: Mesa 26.2.2 applies force_glsl_version to every shader, even ones with an explicit #version. That's what broke glamor's #version 130 shader in gamescope's Xwayland.

Why this option is narrower: it only changes shaders being compiled as GLSL 1.10, which is what a shader with no #version defaults to. For those it allows a few GLSL 1.20 features:
- mixing ints and floats (implicit int→float conversions), which is what your other demo needs
- array constructors
- assigning whole arrays
- arrays as out/inout parameters

Shaders that declare 1.20 or higher, like glamor's, aren't affected, so it's safe for Xwayland to inherit it. I checked this in the Mesa 26.2.2 source (has_implicit_conversions() in glsl_parser_extras.h, plus the version checks in ast_to_hir.cpp and ast_function.cpp). The option also exists in your installed libgallium.

The limit: I haven't tested it on the other demo. If that demo relies on some other GLSL 1.20 feature, this option won't cover it. In that case, keep force_glsl_version=120 but set it on the wine process only, where Xwayland can't inherit it:

gamescope -- env force_glsl_version=120 wine demo.exe

Separately, Mesa's linker crashing on a shader that failed to compile is a Mesa bug you could report upstream, but you don't need a fix for it once the variable is gone.




I updated magellan.txt with the findings and added a wine_cpus meta option, plus an override entry for Magellan. The unit tests pass, and in a headless demarc run the demo process saw 4 CPUs while 
the host has 24. I didn't wait for it to get past its setup dialog in that run, so it isn't tested all the way through demarc yet.                                                                   

magellan.txt: the old "What's now blocking it" section is now a write-up of the font crash: why the BSP root ends up NULL, that the bad mesh is the "mfx" text in Trebuchet MS (wine substitutes Liberation Sans), where to get trebuc32.exe with its checksum, and the things I ruled out (DXVK, wow64). There's also a short "How it was found" section on the winedbg setup and the font trace.

wine_cpus=N:
- How it works: it only works inside the bwrap sandbox. demarc writes 0-(N-1) to a file next to the sandbox's mount point and binds it read-only over /sys/devices/system/cpu/online and /sys/devices/system/cpu/present, which is where wine gets the CPU count.
- Code: it lives in src/wine_sandbox.rs, and src/newsys/windows.rs passes the count through. A value that isn't a positive number is ignored with a warning, and so is the option when the session runs without the sandbox.
- Tests: I added two, one for the bind arguments and one for parsing the value.

Override in system/overrides.toml:
[zoo.829]
file = "fr-063_v1.02.zip"
meta = { wine_cpus = 4 }
I also set file so it downloads v1.02, the bug-fix release, because demozoo lists the v1.01 party version first.
