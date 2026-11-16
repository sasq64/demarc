// Timothy Lottes' public-domain CRT shader (system/shaders/lottes.wgsl), cut
// down for phone GPUs. The frame texture is sRGB, so sampling linearizes; the
// scanlines and a 3x5 bloom share one 3x5 block of fetches. One entry point
// per filter: a branch would make all of them pay for the biggest.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> VertexOutput {
    let uv = vec2<f32>(f32(index >> 1u), f32(index & 1u)) * 2.0;
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return VertexOutput(clip, uv);
}

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

const HARD_SCAN: f32 = -8.0;
const HARD_PIX: f32 = -3.0;
const HARD_BLOOM_SCAN: f32 = -4.0;
const HARD_BLOOM_PIX: f32 = -1.5;
const BLOOM_AMOUNT: f32 = 0.15;
const MASK_DARK: f32 = 0.5;
const MASK_LIGHT: f32 = 1.5;

// exp2(scale * |pos|^1.5)
fn gaus(pos: f32, scale: f32) -> f32 {
    let a = abs(pos);
    return exp2(scale * a * sqrt(a));
}

struct Block {
    base: vec2<f32>,
    d: vec2<f32>,
    size: vec2<f32>,
}

fn block(uv: vec2<f32>) -> Block {
    let size = vec2<f32>(textureDimensions(screen_texture));
    let p = uv * size;
    let base = floor(p);
    return Block(base, vec2<f32>(0.5) - (p - base), size);
}

fn tap(b: Block, x: f32, y: f32) -> vec3<f32> {
    let p = (b.base + vec2<f32>(x + 0.5, y + 0.5)) / b.size;
    return textureSampleLevel(screen_texture, texture_sampler, p, 0.0).rgb;
}

fn h3(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, d: f32, s: f32) -> vec3<f32> {
    let wa = gaus(d - 1.0, s);
    let wb = gaus(d, s);
    let wc = gaus(d + 1.0, s);
    return (a * wa + b * wb + c * wc) / (wa + wb + wc);
}

fn h5(a: vec3<f32>, b: vec3<f32>, c: vec3<f32>, e: vec3<f32>, f: vec3<f32>, d: f32, s: f32) -> vec3<f32> {
    let wa = gaus(d - 2.0, s);
    let wb = gaus(d - 1.0, s);
    let wc = gaus(d, s);
    let we = gaus(d + 1.0, s);
    let wf = gaus(d + 2.0, s);
    return (a * wa + b * wb + c * wc + e * we + f * wf) / (wa + wb + wc + we + wf);
}

fn mask(pos: vec2<f32>) -> vec3<f32> {
    var m = vec3<f32>(MASK_DARK);
    let x = fract(pos.x * 0.333333333);
    if x < 0.333 {
        m.r = MASK_LIGHT;
    } else if x < 0.666 {
        m.g = MASK_LIGHT;
    } else {
        m.b = MASK_LIGHT;
    }
    return m;
}

@fragment
fn fragment_none(in: VertexOutput) -> @location(0) vec4<f32> {
    return vec4<f32>(textureSampleLevel(screen_texture, texture_sampler, in.uv, 0.0).rgb, 1.0);
}

@fragment
fn fragment_normal(in: VertexOutput) -> @location(0) vec4<f32> {
    let b = block(in.uv);
    let top = h3(tap(b, -1.0, -1.0), tap(b, 0.0, -1.0), tap(b, 1.0, -1.0), b.d.x, HARD_PIX);
    let mid = h5(tap(b, -2.0, 0.0), tap(b, -1.0, 0.0), tap(b, 0.0, 0.0), tap(b, 1.0, 0.0),
        tap(b, 2.0, 0.0), b.d.x, HARD_PIX);
    let bot = h3(tap(b, -1.0, 1.0), tap(b, 0.0, 1.0), tap(b, 1.0, 1.0), b.d.x, HARD_PIX);
    let color = top * gaus(b.d.y - 1.0, HARD_SCAN) + mid * gaus(b.d.y, HARD_SCAN)
        + bot * gaus(b.d.y + 1.0, HARD_SCAN);
    // Linear out; the sRGB render target encodes on write.
    return vec4<f32>(color * mask(in.position.xy * 1.000001), 1.0);
}

@fragment
fn fragment_full(in: VertexOutput) -> @location(0) vec4<f32> {
    let b = block(in.uv);
    let t0 = tap(b, -2.0, -1.0);
    let t1 = tap(b, -1.0, -1.0);
    let t2 = tap(b, 0.0, -1.0);
    let t3 = tap(b, 1.0, -1.0);
    let t4 = tap(b, 2.0, -1.0);
    let m0 = tap(b, -2.0, 0.0);
    let m1 = tap(b, -1.0, 0.0);
    let m2 = tap(b, 0.0, 0.0);
    let m3 = tap(b, 1.0, 0.0);
    let m4 = tap(b, 2.0, 0.0);
    let b0 = tap(b, -2.0, 1.0);
    let b1 = tap(b, -1.0, 1.0);
    let b2 = tap(b, 0.0, 1.0);
    let b3 = tap(b, 1.0, 1.0);
    let b4 = tap(b, 2.0, 1.0);
    let dx = b.d.x;
    let dy = b.d.y;
    let scan = h3(t1, t2, t3, dx, HARD_PIX) * gaus(dy - 1.0, HARD_SCAN)
        + h5(m0, m1, m2, m3, m4, dx, HARD_PIX) * gaus(dy, HARD_SCAN)
        + h3(b1, b2, b3, dx, HARD_PIX) * gaus(dy + 1.0, HARD_SCAN);
    let bloom = h5(t0, t1, t2, t3, t4, dx, HARD_BLOOM_PIX) * gaus(dy - 1.0, HARD_BLOOM_SCAN)
        + h5(m0, m1, m2, m3, m4, dx, HARD_BLOOM_PIX) * gaus(dy, HARD_BLOOM_SCAN)
        + h5(b0, b1, b2, b3, b4, dx, HARD_BLOOM_PIX) * gaus(dy + 1.0, HARD_BLOOM_SCAN);
    let color = scan + bloom * BLOOM_AMOUNT;
    return vec4<f32>(color * mask(in.position.xy * 1.000001), 1.0);
}
