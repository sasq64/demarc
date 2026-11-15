// Original by FabriceNeyret2 in 2015-11-12

struct FullscreenVertexOutput {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}
struct Ui {
    resolution: vec2f,
    origin: vec2f,
    screen: vec2f,
    time: f32,
    _pad: f32}
@group(0) @binding(0) var<uniform> ui: Ui;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

// D: duration of advection layers, Z: zoom factor
const D: f32 = 8.0;
const Z: f32 = 3.0;

fn rnd(u: vec2f, d: f32) -> vec2f {
    return fract(1e4 * sin(u * mat2x2f(1234.0, -53.0, 457.0, -17.0) + d));
}

fn layer(u0: vec2f, t: f32) -> f32 {
    let u = u0 / (exp2(t - 8.0) * D / (3.0 + t));
    let iu = ceil(u);
    let p = 0.2 + 0.6 * rnd(iu, 0.0);
    let r = 9.0 * rnd(iu, 1.0).x;
    if r > 1.0 {
        return 1.0;
    }
    return length(p - fract(u)) * 8.0 / (1.0 + 5.0 * r);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {
    let u = vec2f(in.uv.x, 1.0 - in.uv.y) * ui.resolution / ui.resolution.y - 0.5;

    let p = vec3f(-1.0, 0.0, 1.0) / 3.0;
    var t = fract(ui.time / D + p + 0.5) - 0.5;
    let w = 0.5 + 0.5 * cos(6.28 * t);
    t = t * D + Z;

    var l = vec3f(layer(u, t.x), layer(-u, t.y), layer(u.yx, t.z));
    l = 0.03 / (l * l);

    // The target is sRGB; Shadertoy output is already display-encoded.
    return vec4f(vec3f(pow(dot(w, l), 2.2)), 1.0);
}
