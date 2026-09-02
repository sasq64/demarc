// Simple single-pass LCD grid shader, styled for handheld displays such as the
// Game Boy. Original WGSL implementation for demarc — not derived from any GPL
// source. Lightweight alternative to the more elaborate, drop-shadow `lcd.wgsl`.
// Drop-in alternative to `lottes.wgsl`: it shares the same bindings, uniform
// layout, and `crt_enabled` passthrough behaviour, so the post-process pipeline
// can swap between them without any other changes.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct PostProcessUniform {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    crt_enabled: u32,
    // Opacity of this view, blended against the view target. Only
    // `--cross-fade` sets it below 1, to fade one emulator over another.
    alpha: f32,
}
@group(0) @binding(2) var<uniform> settings: PostProcessUniform;

// How dark the gaps between LCD pixels get (0 = no grid, 1 = black grid).
const GRID_STRENGTH: f32 = 0.20;
// Fraction of each pixel cell taken up by the gap, per axis (0..0.5).
const GAP_WIDTH: f32 = 0.12;
// Softness of the grid edge, in cell fractions. Larger = smoother gaps.
const GAP_SOFTNESS: f32 = 0.06;
// Brightness boost to compensate for the darkening introduced by the grid.
const BRIGHT_BOOST: f32 = 1.0;
// Subtle darkening towards the lower-right of each pixel, mimicking the shadow
// cast by the raised cell wall on a real reflective LCD (0 = off).
const SHADE_AMOUNT: f32 = 0.10;

// Convert the raw gamma-encoded source sample to linear. The screen texture
// holds display-ready sRGB values, and the render target is an sRGB texture
// that encodes on write — writing without decoding first would gamma-encode
// twice and wash the image out (see blit.wgsl for the full story).
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lower = c / 12.92;
    let higher = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(higher, lower, c <= vec3<f32>(0.04045));
}

// Grid factor for one axis: ~1.0 in the pixel body, dropping towards 0 in the
// gap between cells. `f` is the fractional position within the cell [0,1).
fn axis_grid(f: f32) -> f32 {
    let d = min(f, 1.0 - f);
    return smoothstep(GAP_WIDTH - GAP_SOFTNESS, GAP_WIDTH + GAP_SOFTNESS, d);
}

fn lcd(uv: vec2<f32>, source_size: vec2<f32>) -> vec3<f32> {
    let p = uv * source_size;
    let texel = floor(p);
    let f = p - texel;

    // Nearest-texel colour. Sampling at the cell centre yields the exact texel
    // even with a linear sampler, so the LCD dots stay crisp.
    let center = (texel + vec2<f32>(0.5)) / source_size;
    let color = textureSampleLevel(screen_texture, texture_sampler, center, 0.0).rgb;

    // Grid mask: darken near the cell edges to suggest the gaps between the
    // physical LCD pixels.
    let grid = axis_grid(f.x) * axis_grid(f.y);
    var mask = mix(1.0 - GRID_STRENGTH, 1.0, grid);

    // Soft directional shade across each pixel body for a bit of LCD relief.
    mask = mask * (1.0 - SHADE_AMOUNT * (f.x + f.y) * 0.5);

    // Grid/shade math runs in gamma space (like the libretro LCD shaders);
    // decode only at the end so the sRGB target's encode restores the original.
    return srgb_to_linear(color * mask * BRIGHT_BOOST);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mapped_uv = (in.uv - settings.uv_offset) / settings.uv_scale;

    // Plain passthrough when the effect is disabled — mirrors lottes.wgsl so the
    // two shaders behave identically with `crt_enabled == 0`.
    if settings.crt_enabled == 0u {
        let c = textureSampleLevel(screen_texture, texture_sampler, mapped_uv, 0.0).rgb;
        return vec4<f32>(srgb_to_linear(c), settings.alpha);
    }

    let source_size = vec2<f32>(textureDimensions(screen_texture));
    let color = lcd(mapped_uv, source_size);
    return vec4<f32>(color, settings.alpha);
}
