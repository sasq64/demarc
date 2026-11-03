// Writes the 6x target at phase n/6 between A and B. Every pixel is a colour from
// A or B, never a mix. With DEBUG set it writes which path decided the pixel.
// Tiles whose changes the vectors mostly fail to explain show the nearest frame
// throughout, and so does a share of each 6x6 block as the frame's trust falls.

const S: i32 = 6;
const TILE = vec2<i32>(16, 8);
override DEBUG: bool = false;
struct IP { a: vec4<u32>, b: vec4<u32> }; // a=(layer_a, layer_b, n, _), b=(tiles_x, tiles_y, _, _)
@group(0) @binding(0) var frames: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read> vecs: array<vec4<i32>>; // (v, rigid, _)
@group(0) @binding(2) var<uniform> u: IP;
@group(0) @binding(3) var<storage, read> conf: array<u32>; // as me.wgsl has it

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(vec2<u32>((i << 1u) & 2u, i & 2u));
    return vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
fn ld(l: u32, q6: vec2<i32>) -> vec3<f32> {
    let dims = vec2<i32>(textureDimensions(frames));
    return textureLoad(frames, clamp(q6 / S, vec2(0), dims - 1), l, 0).rgb;
}
fn eq(x: vec3<f32>, y: vec3<f32>) -> bool { return all(abs(x - y) < vec3(0.5 / 255.0)); }
fn tile_vec(t: vec2<i32>) -> vec4<i32> {
    let c = clamp(t, vec2(0), vec2<i32>(u.b.xy) - 1);
    return vecs[u32(c.y) * u.b.x + u32(c.x)];
}
fn dither(p: vec2<i32>) -> u32 {
    let m2 = array<u32, 4>(0u, 2u, 3u, 1u);
    let m3 = array<u32, 9>(0u, 7u, 3u, 6u, 5u, 2u, 4u, 1u, 8u);
    let q = vec2<u32>(p % S);
    return 9u * m2[(q.y % 2u) * 2u + q.x % 2u] + m3[(q.y / 2u) * 3u + q.x / 2u];
}
fn doubtful(tc: vec2<i32>) -> bool {
    let c = conf[4u + u32(tc.y) * u.b.x + u32(tc.x)];
    let changed = c & 0xffffu;
    return changed >= 16u && (c >> 16u) * 2u < changed;
}
fn out(c: vec3<f32>, path: u32) -> vec4<f32> {
    if (DEBUG) { return vec4(f32(path) / 255.0, 0.0, 0.0, 1.0); }
    return vec4(c, 1.0);
}
@fragment
fn fs_interp(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(pos.xy);
    let n = i32(u.a.z);
    let A0 = ld(u.a.x, p);
    if (n == 0) { return out(A0, 0u); }
    let B0 = ld(u.a.y, p);
    let tc = p / (S * TILE);
    let trust = 256u - conf[2];
    if (doubtful(tc) || dither(p) * 256u + 128u >= trust * 36u) {
        return out(select(B0, A0, n < 3), select(4u, 0u, eq(A0, B0)));
    }
    let own = tile_vec(tc);
    if (eq(A0, B0)) {
        // Unchanged, but a stroke can still cross it between the two frames.
        if (any(own.xy != vec2(0))) {
            let q = p - n * own.xy;
            let r = p + (S - n) * own.xy;
            let a = ld(u.a.x, q);
            let b = ld(u.a.y, r);
            let moved = own.z != 0 || !eq(a, ld(u.a.y, q)) || !eq(ld(u.a.x, r), b);
            if (moved && eq(a, b) && !eq(a, A0)) { return out(a, 1u); }
        }
        return out(A0, 0u);
    }
    var c = array<vec2<i32>, 5>(own.xy,
        tile_vec(tc + vec2(1, 0)).xy, tile_vec(tc - vec2(1, 0)).xy,
        tile_vec(tc + vec2(0, 1)).xy, tile_vec(tc - vec2(0, 1)).xy);
    var vo = vec2(0, 0);
    for (var i = 0; i < 5; i++) {
        let v = c[i];
        if (all(v == vec2(0))) { continue; }
        if (all(vo == vec2(0))) { vo = v; }
        let a = ld(u.a.x, p - n * v);
        let b = ld(u.a.y, p + (S - n) * v);
        if (eq(a, b)) { return out(a, 1u); }
    }
    if (any(vo != vec2(0))) {
        if (eq(A0, ld(u.a.y, p + S * vo))) { return out(B0, 2u); }
        if (eq(B0, ld(u.a.x, p - S * vo))) { return out(A0, 2u); }
    }
    return out(select(B0, A0, n < 3), 3u);
}
