// GAME BOY DOT MATRIX SHADER — single-pass WGSL port.
//
// Ported from the libretro "Game Boy Dot Matrix Shader v1.2" (gameboy-pocket),
// files handheld/shaders/gameboy/shader-files/gb-pass0.slang and gb-pass4.slang.
//
//   Copyright (C) 2013 Harlequin  <unknown92835@gmail.com>
//   Copyright (C) 2024-2026 Matt Akins
//   Copyright (C) 2026 demarc — single-pass WGSL adaptation
//
// This program is free software: you can redistribute it and/or modify it under
// the terms of the GNU General Public License as published by the Free Software
// Foundation, either version 3 of the License, or (at your option) any later
// version. This program is distributed in the hope that it will be useful, but
// WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
// FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
// details. You should have received a copy of the GNU General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.
//
// NOTE ON LICENSING: unlike the other shaders shipped with demarc (lottes.wgsl,
// dim.wgsl, lcd_simple.wgsl) this file is GPL v3 because it is a derivative of
// the GPL'd upstream shader.
//
// PORTING NOTES: the original is a five-pass chain (geometric dot generation +
// frame-history response-time + a separable blur of the shadow buffer + final
// compositing). This single-pass version keeps the two visually defining parts —
// the anti-aliased geometric dot coverage of gb-pass0 and the drop-shadow of
// gb-pass4 — and drops the parts that require extra render targets (multi-frame
// LCD ghosting, and the separately-blurred shadow buffer).
//
// It also departs from the original's *reflective* model (dark ink dots on a
// fixed light-green "paper" background, opacity driven by pixel brightness). As
// a general-purpose frontend LCD it keeps the source colours and derives the
// inter-pixel gap from each pixel's OWN colour (a darkened grid line), so black
// pixels stay black instead of leaking the paper colour through the gaps. The
// drop shadow is a directional darkening confined to the gap on the lower-right
// of each lit pixel (analytic, sub-texel offset of the periodic dot field),
// giving the raised-pixel look without a separate blurred shadow buffer.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct PostProcessUniform {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    crt_enabled: u32,
}
@group(0) @binding(2) var<uniform> settings: PostProcessUniform;

// --- Dot geometry (gb-pass0 defaults) ---------------------------------------
// Size of the lit dot within its cell, 0..1 (`pixel_size`). A little room is
// left around the dot so the drop shadow has somewhere to pool.
const PIXEL_SIZE: f32 = 0.82;
// Dot shape: 0 = circular, 1 = rectangular (`pixel_shape`, clamped to 0..1).
// Rounded dots read as raised cells far better than a hard square grid.
const PIXEL_SHAPE: f32 = 0.10;
// Extra edge softening added to the automatic (derivative-based) anti-aliasing.
const EDGE_SOFTNESS: f32 = 0.13;

// --- Grid & shadow ----------------------------------------------------------
// Brightness of the inter-pixel gap relative to the pixel colour (0 = black
// grid, 1 = no grid). Deriving the gap from the pixel keeps blacks black.
const GAP_BRIGHTNESS: f32 = 0.75;
// How strongly the drop shadow darkens the background behind the dot
// (`shadow_opacity`).
const SHADOW_OPACITY: f32 = 0.80;
// Shadow displacement in cell fractions, down-right (light from upper-left).
// Single-pass uses a sub-texel offset of the analytic dot field, so this is a
// fraction of a cell rather than the full-cell offset the multi-pass original
// can afford.
const SHADOW_OFFSET: vec2<f32> = vec2<f32>(0.28, 0.28);
// Softness of the shadow edge (cell fractions) — a wide, soft penumbra so the
// shadow reads as a cast shadow rather than a second grid line. Stands in for
// the original's dedicated blur passes.
const SHADOW_BLUR: f32 = 0.24;

// Anti-aliased coverage of the dot centred in a cell, for a sample at fractional
// position `f` (in 0..1) within that cell. `aa` is the edge softness per axis.
// Mirrors gb-pass0's blend between a rectangular (separable) and circular
// (radial) dot via `pixel_shape`.
fn dot_coverage(f: vec2<f32>, aa: vec2<f32>) -> f32 {
    let d = abs(f - vec2<f32>(0.5));
    let h = PIXEL_SIZE * 0.5;
    // Rectangular: independent membership on each axis.
    let rx = smoothstep(h + aa.x, h - aa.x, d.x);
    let ry = smoothstep(h + aa.y, h - aa.y, d.y);
    let rect = rx * ry;
    // Circular: radial membership.
    let r = length(d);
    let aa_r = max(aa.x, aa.y);
    let circ = smoothstep(h + aa_r, h - aa_r, r);
    return mix(circ, rect, clamp(PIXEL_SHAPE, 0.0, 1.0));
}

fn lcd(uv: vec2<f32>, source_size: vec2<f32>) -> vec3<f32> {
    // Position in texel (cell) space, and the per-pixel footprint used to size
    // the anti-aliasing. `fwidth` of the continuous `tx` gives ~texels-per-pixel,
    // so dots stay crisp when magnified and soften as they approach 1:1.
    let tx = uv * source_size;
    let aa = fwidth(tx) * 0.5 + vec2<f32>(EDGE_SOFTNESS);

    // Crisp source colour: sample at the current cell's centre.
    let cell = floor(tx);
    let center = (cell + vec2<f32>(0.5)) / source_size;
    let col = textureSampleLevel(screen_texture, texture_sampler, center, 0.0).rgb;

    // Coverage of this cell's lit pixel body.
    let f = fract(tx);
    let cov = dot_coverage(f, aa);

    // Shadow cast by the dot onto the background down-and-right of it: the same
    // dot shape, offset (so it pokes out past the dot toward the lower-right)
    // and heavily softened into a penumbra.
    let shadow_cov = dot_coverage(fract(tx - SHADOW_OFFSET), aa + vec2<f32>(SHADOW_BLUR));

    // Compose back-to-front: the inter-pixel gap (a darkened shade of the
    // pixel's own colour — keeps blacks black), then the drop shadow darkening
    // that background, then the lit dot painted on top of its own shadow.
    let gap = col * GAP_BRIGHTNESS;
    let background = gap * (1.0 - shadow_cov * SHADOW_OPACITY);
    let out_color = mix(background, col, cov);
    return out_color;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mapped_uv = (in.uv - settings.uv_offset) / settings.uv_scale;

    // Plain passthrough when the effect is disabled — mirrors lottes.wgsl so the
    // shaders behave identically with `crt_enabled == 0`.
    if settings.crt_enabled == 0u {
        let c = textureSampleLevel(screen_texture, texture_sampler, mapped_uv, 0.0).rgb;
        return vec4<f32>(c, 1.0);
    }

    let source_size = vec2<f32>(textureDimensions(screen_texture));
    let color = lcd(mapped_uv, source_size);
    return vec4<f32>(color, 1.0);
}
