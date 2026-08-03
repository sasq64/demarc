TAGS

Arg tags + system default tags => tags

prepare(tags) -> WorkFile with tags




 
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

PROFILING BEVY

`--features profile` (src/profiling.rs) turns on three things at once: Bevy's
per-system tracing spans written to a Chrome/Perfetto trace, the frame/CPU/entity
diagnostics, and a per-second "change audit" that counts how many entities had
each hot component marked changed. The audit is the cheap way to spot a system
that rewrites a component every frame whether or not the value moved — a count
sitting at exactly 1.00/f per entity is the tell.

```
just profile demos/rebels.adf     # build + run, writes trace.json
just trace-summary                # rank spans by self time
```

The trace is roughly 100 MB per second of capture, so keep runs short. The
summary script is what actually answers "what costs the most"; Perfetto is for
looking at one frame's shape. Note that a *schedule's* self time is inflated —
its systems run on worker threads, so they aren't subtracted as children. Only
`system:` spans are directly comparable.

Baseline, 2026-07-31, rebels.adf windowed at 720x540 on a 144 Hz screen, 1.4k
frames: ~7.4 ms/frame, no single main-world system above 0.2 ms. The app is
presentation-bound, not CPU-bound — the wait shows up as `prepare_windows` self
time. What the capture did show:

- Dropping bevy's `3d` feature removed ~1 ms/frame of PBR/light/anti-alias
  systems that a 2D-only app never uses (135 -> 160 fps, 2388 -> 1497 distinct
  spans). Done: `default-features = false, features = ["2d", "ui", "audio"]`.
- `Image` assets are marked modified once per *app* frame (1.00/f), but a PAL
  core only produces 50 new frames a second. `run_retro` calls
  `images.get_mut()` before it knows whether the core stepped, so ~2/3 of the
  texture uploads (`prepare_assets<GpuImage>`, 0.15 ms/frame plus ~1.6 MB of
  bus traffic each) re-upload pixels that didn't change.
- `PostProcess`, `PostProcessUniform` and `BorderScissor` are also rewritten
  1.00/f per camera; `update_post_process_uniform` re-inserts `BorderScissor`
  through `Commands` every frame instead of comparing first.
