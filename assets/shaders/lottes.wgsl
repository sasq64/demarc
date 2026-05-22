// PUBLIC DOMAIN CRT STYLED SCAN-LINE SHADER by Timothy Lottes
// Ported from GLSL to WGSL.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct PostProcessUniform {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
}
@group(0) @binding(2) var<uniform> settings: PostProcessUniform;

const HARD_SCAN: f32 = -8.0;
const HARD_PIX: f32 = -3.0;
const WARP_X: f32 = 0.011;
const WARP_Y: f32 = 0.011;
// const WARP_X: f32 = 0.031; 
// const WARP_Y: f32 = 0.041;
const MASK_DARK: f32 = 0.5;
const MASK_LIGHT: f32 = 1.5;
const SCALE_IN_LINEAR_GAMMA: f32 = 1.0;
const SHADOW_MASK: f32 = 3.0;
const BRIGHT_BOOST: f32 = 1.0;
const HARD_BLOOM_PIX: f32 = -1.5;
const HARD_BLOOM_SCAN: f32 = -2.0;
const BLOOM_AMOUNT: f32 = 0.15;
const SHAPE: f32 = 2.0;

fn to_linear1(c: f32) -> f32 {
    if SCALE_IN_LINEAR_GAMMA == 0.0 {
        return c;
    }
    if c <= 0.04045 {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    if SCALE_IN_LINEAR_GAMMA == 0.0 {
        return c;
    }
    return vec3<f32>(to_linear1(c.r), to_linear1(c.g), to_linear1(c.b));
}

fn to_srgb1(c: f32) -> f32 {
    if SCALE_IN_LINEAR_GAMMA == 0.0 {
        return c;
    }
    if c < 0.0031308 {
        return c * 12.92;
    }
    return 1.055 * pow(c, 0.41666) - 0.055;
}

fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    if SCALE_IN_LINEAR_GAMMA == 0.0 {
        return c;
    }
    return vec3<f32>(to_srgb1(c.r), to_srgb1(c.g), to_srgb1(c.b));
}

fn fetch(pos: vec2<f32>, off: vec2<f32>, source_size: vec2<f32>) -> vec3<f32> {
    let p = (floor(pos * source_size + off) + vec2<f32>(0.5, 0.5)) / source_size;
    let s = textureSampleLevel(screen_texture, texture_sampler, p, 0.0).rgb;
    return to_linear(BRIGHT_BOOST * s);
}

fn dist_to_texel(pos: vec2<f32>, source_size: vec2<f32>) -> vec2<f32> {
    let p = pos * source_size;
    return -((p - floor(p)) - vec2<f32>(0.5));
}

fn gaus(pos: f32, scale: f32) -> f32 {
    return exp2(scale * pow(abs(pos), SHAPE));
}

fn horz3(pos: vec2<f32>, off: f32, source_size: vec2<f32>) -> vec3<f32> {
    let b = fetch(pos, vec2<f32>(-1.0, off), source_size);
    let c = fetch(pos, vec2<f32>( 0.0, off), source_size);
    let d = fetch(pos, vec2<f32>( 1.0, off), source_size);
    let dst = dist_to_texel(pos, source_size).x;
    let scale = HARD_PIX;
    let wb = gaus(dst - 1.0, scale);
    let wc = gaus(dst + 0.0, scale);
    let wd = gaus(dst + 1.0, scale);
    return (b * wb + c * wc + d * wd) / (wb + wc + wd);
}

fn horz5(pos: vec2<f32>, off: f32, source_size: vec2<f32>) -> vec3<f32> {
    let a = fetch(pos, vec2<f32>(-2.0, off), source_size);
    let b = fetch(pos, vec2<f32>(-1.0, off), source_size);
    let c = fetch(pos, vec2<f32>( 0.0, off), source_size);
    let d = fetch(pos, vec2<f32>( 1.0, off), source_size);
    let e = fetch(pos, vec2<f32>( 2.0, off), source_size);
    let dst = dist_to_texel(pos, source_size).x;
    let scale = HARD_PIX;
    let wa = gaus(dst - 2.0, scale);
    let wb = gaus(dst - 1.0, scale);
    let wc = gaus(dst + 0.0, scale);
    let wd = gaus(dst + 1.0, scale);
    let we = gaus(dst + 2.0, scale);
    return (a * wa + b * wb + c * wc + d * wd + e * we) / (wa + wb + wc + wd + we);
}

fn horz7(pos: vec2<f32>, off: f32, source_size: vec2<f32>) -> vec3<f32> {
    let a = fetch(pos, vec2<f32>(-3.0, off), source_size);
    let b = fetch(pos, vec2<f32>(-2.0, off), source_size);
    let c = fetch(pos, vec2<f32>(-1.0, off), source_size);
    let d = fetch(pos, vec2<f32>( 0.0, off), source_size);
    let e = fetch(pos, vec2<f32>( 1.0, off), source_size);
    let f = fetch(pos, vec2<f32>( 2.0, off), source_size);
    let g = fetch(pos, vec2<f32>( 3.0, off), source_size);
    let dst = dist_to_texel(pos, source_size).x;
    let scale = HARD_BLOOM_PIX;
    let wa = gaus(dst - 3.0, scale);
    let wb = gaus(dst - 2.0, scale);
    let wc = gaus(dst - 1.0, scale);
    let wd = gaus(dst + 0.0, scale);
    let we = gaus(dst + 1.0, scale);
    let wf = gaus(dst + 2.0, scale);
    let wg = gaus(dst + 3.0, scale);
    return (a * wa + b * wb + c * wc + d * wd + e * we + f * wf + g * wg)
        / (wa + wb + wc + wd + we + wf + wg);
}

fn scan(pos: vec2<f32>, off: f32, source_size: vec2<f32>) -> f32 {
    let dst = dist_to_texel(pos, source_size).y;
    return gaus(dst + off, HARD_SCAN);
}

fn bloom_scan(pos: vec2<f32>, off: f32, source_size: vec2<f32>) -> f32 {
    let dst = dist_to_texel(pos, source_size).y;
    return gaus(dst + off, HARD_BLOOM_SCAN);
}

fn tri(pos: vec2<f32>, source_size: vec2<f32>) -> vec3<f32> {
    let a = horz3(pos, -1.0, source_size);
    let b = horz5(pos,  0.0, source_size);
    let c = horz3(pos,  1.0, source_size);
    let wa = scan(pos, -1.0, source_size);
    let wb = scan(pos,  0.0, source_size);
    let wc = scan(pos,  1.0, source_size);
    return a * wa + b * wb + c * wc;
}

fn bloom(pos: vec2<f32>, source_size: vec2<f32>) -> vec3<f32> {
    let a = horz5(pos, -2.0, source_size);
    let b = horz7(pos, -1.0, source_size);
    let c = horz7(pos,  0.0, source_size);
    let d = horz7(pos,  1.0, source_size);
    let e = horz5(pos,  2.0, source_size);
    let wa = bloom_scan(pos, -2.0, source_size);
    let wb = bloom_scan(pos, -1.0, source_size);
    let wc = bloom_scan(pos,  0.0, source_size);
    let wd = bloom_scan(pos,  1.0, source_size);
    let we = bloom_scan(pos,  2.0, source_size);
    return a * wa + b * wb + c * wc + d * wd + e * we;
}

fn warp(pos_in: vec2<f32>) -> vec2<f32> {
    var p = pos_in * 2.0 - 1.0;
    p = p * vec2<f32>(1.0 + (p.y * p.y) * WARP_X, 1.0 + (p.x * p.x) * WARP_Y);
    return p * 0.5 + 0.5;
}

fn mask(pos_in: vec2<f32>) -> vec3<f32> {
    var m = vec3<f32>(MASK_DARK, MASK_DARK, MASK_DARK);
    var pos = pos_in;

    if SHADOW_MASK == 1.0 {
        var line_val = MASK_LIGHT;
        var odd = 0.0;
        if fract(pos.x * 0.166666666) < 0.5 { odd = 1.0; }
        if fract((pos.y + odd) * 0.5) < 0.5 { line_val = MASK_DARK; }
        pos.x = fract(pos.x * 0.333333333);
        if      pos.x < 0.333 { m.r = MASK_LIGHT; }
        else if pos.x < 0.666 { m.g = MASK_LIGHT; }
        else                  { m.b = MASK_LIGHT; }
        m = m * line_val;
    } else if SHADOW_MASK == 2.0 {
        pos.x = fract(pos.x * 0.333333333);
        if      pos.x < 0.333 { m.r = MASK_LIGHT; }
        else if pos.x < 0.666 { m.g = MASK_LIGHT; }
        else                  { m.b = MASK_LIGHT; }
    } else if SHADOW_MASK == 3.0 {
        pos.x = pos.x + pos.y * 3.0;
        pos.x = fract(pos.x * 0.166666666);
        if      pos.x < 0.333 { m.r = MASK_LIGHT; }
        else if pos.x < 0.666 { m.g = MASK_LIGHT; }
        else                  { m.b = MASK_LIGHT; }
    } else if SHADOW_MASK == 4.0 {
        pos = floor(pos * vec2<f32>(1.0, 0.5));
        pos.x = pos.x + pos.y * 3.0;
        pos.x = fract(pos.x * 0.166666666);
        if      pos.x < 0.333 { m.r = MASK_LIGHT; }
        else if pos.x < 0.666 { m.g = MASK_LIGHT; }
        else                  { m.b = MASK_LIGHT; }
    }

    return m;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mapped_uv = (in.uv - settings.uv_offset) / settings.uv_scale;
    if any(mapped_uv < vec2<f32>(0.0)) || any(mapped_uv > vec2<f32>(1.0)) {
        return vec4<f32>(0.0, 0.0, 0.0, 1.0);
    }

    let source_size = vec2<f32>(textureDimensions(screen_texture));
    let pos = warp(mapped_uv);
    var out_color = tri(pos, source_size);

    out_color = out_color + bloom(pos, source_size) * BLOOM_AMOUNT;

    if SHADOW_MASK > 0.0 {
        out_color = out_color * mask(in.position.xy * 1.000001);
    }

    return vec4<f32>(to_srgb(out_color), 1.0);
}
