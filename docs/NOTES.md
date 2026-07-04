RetroEmu

-> Load library
-> Set callbacks
<- Set default variables
-> retro_init
<- Get variables
-> load_game

 
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
