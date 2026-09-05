# Shader presets

`--shader` picks one of the built-in shaders; `--slangp <path>` runs any RetroArch
`.slangp` preset through librashader instead. The bundled ones live in
`system/shaders/slangp/` (a small subset of the slang-shaders repo), but the flag
takes any path, which is how the big bezel packs get used.

## The librashader fork

`Cargo.toml` pins `librashader` to a branch of a fork rather than the crates.io
release, because stock librashader 0.11 cannot parse the presets the Mega Bezel
packs ship, nor compile a shader that calls `modf()`:

* <https://github.com/sasq64/librashader> branch `demarc`,
  four commits on top of `librashader-v0.11.4`.

### Preset parsing

Three fixes, each matching what RetroArch's own parser does:

| Fix | Stock behaviour | Why it matters |
|---|---|---|
| A `#reference` path ends at its closing quote | The rest of the line, comment and quotes included, became part of the path | Mega Bezel packs annotate nearly every reference: `#reference "../base/preset.params" // basic setup` |
| Reference depth counts chain levels | A single counter was bumped once per preset read, so any chain touching more than 16 *files* failed with `ExceededReferenceDepth` | A pack preset pulls in ~45 files while never going more than three levels deep |
| An unresolvable reference is skipped | The whole preset failed with an IO error | Shipped packs reference files that were renamed or never shipped; RetroArch only warns |

`src/tests/post_process_tests.rs::preset_references_follow_retroarch_rules` pins
all three, so moving the dependency back to a stock librashader fails the test
suite rather than every bezel preset at load time.

Note that a preset whose references *all* fail to resolve now parses into zero
passes and renders nothing, instead of reporting an error. That is RetroArch's
behaviour too, and the ignored test below catches it for an installed pack.

### `modf()`

The fourth commit registers the `modf`/`frexp` result types that naga's SPIR-V
frontend forgets. naga lowers `OpExtInst ModfStruct` to `Expression::Math { fun:
Modf, .. }` but never calls `Module::generate_predeclared_type` for the struct it
returns, so validation fails with `MissingSpecialType` and takes down every
shader calling `modf()` — fourteen of them in slang-shaders, the gameboy and
authentic_gbc handhelds and several xBR passes among them. librashader walks each
module after parsing and registers what is missing; nothing else about those
shaders needed changing.

This used to be worked around by patching the shaders themselves (a `modf_`
helper built out of `trunc`), which had to be redone after every slang-shaders
update and quietly changed the result for negative inputs. Don't reintroduce it.

The unrelated workaround still in the tree *is* the matrix varyings: WGSL forbids
aggregate types at the user-defined I/O interface, so a `mat3`/`mat4` passed from
the vertex to the fragment stage (`handheld/shaders/color/*.slang`,
`bezel/scanline-classic/shaders/composite-demod.slang`) has to be moved into the
fragment stage by hand. librashader splits array varyings for this reason
(`split_io_arrays`) but not matrix ones.

### Updating

To update librashader: rebase the branch onto the new tag, push, and change the
`rev` in `Cargo.toml`. Upstream is <https://github.com/SnowflakePowered/librashader>;
if the fixes land there, drop the fork and go back to the crates.io release. The
`modf` fix belongs in naga rather than librashader, so it may arrive from that
direction instead — check whether `naga::front::spv` registers the type before
carrying the commit forward.

## Mega Bezel packs

The bezel packs reach the Mega Bezel shader through a fixed relative path
(`../../../../../../../shaders_slang/bezel/Mega_Bezel/...`), so they only work
from the directory layout RetroArch uses: the pack two levels below a directory
that also holds `shaders_slang`. In this checkout, `shaders/` (gitignored) has it:

```
shaders/
  shaders_slang -> ../slang-shaders     # the slang-shaders checkout, which carries Mega_Bezel
  Mega_Bezel_Packs/
    TheNamec-Commodore/                 # https://github.com/TheNamec/megabezel-commodore-pack
```

```sh
demarc demos/rebels.adf --slangp \
  shaders/Mega_Bezel_Packs/TheNamec-Commodore/presets/Commodore_Amiga500/Commodore_C1084/NMC_SOFT_RGB/FULLDEVICE_FLAT_NIGHT.slangp
```

Presets are named `<machine>/<monitor>/<flavour>/<scaling>_<curvature>_<lighting>.slangp`;
the pack's `README.md` explains what each one does.

### Pack fixups

`scripts/fix-megabezel-pack.sh` repairs three broken references in TheNamec RC4.1
that RetroArch silently swallows (and so, now, does demarc — the presets load
either way, they just lose whatever the missing file contributed):

* four monitors are referenced as `Commodore_Commodore_*` but shipped as
  `Commodore_*`, which costs the `NMC_SOFT_SMOOTH-SUPER-XBR` presets their device setup;
* `crt_spice/gtu/sharp/preset.params` is shipped as `preset .params`, with a space,
  which costs the `NMC_SOFT_RGB` flavours their GTU sharpness pass;
* the `NMC_SOFT_VECTOR` connector points at `Presets/Variations/Vector-Horizontal__STD.slangp`,
  renamed in Mega Bezel V1.7.0 (after the pack's release), which leaves those
  presets with no passes at all.

Re-run it after updating the pack or slang-shaders, then
`cargo test megabezel_pack_presets_resolve -- --ignored` to check a sample of the
whole pack still parses.
