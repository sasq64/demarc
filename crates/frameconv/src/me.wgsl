// One vector per 16x16 tile for the source pair A -> B. Only pixels that changed
// are scored: static ones would outvote a thin moving edge on a textured
// background. A changed pixel costs 1 if neither side of it matches along v (one
// side is occluded, so one is enough), and 1 more if v lands it in the changed
// band, which is how the speed is found on a uniform background.

const R: i32 = 4;
const NC: u32 = 17u;
struct MeP { la: u32, lb: u32, tiles_x: u32, _p: u32 };
@group(0) @binding(0) var frames: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read_write> vecs: array<vec2<i32>>;
@group(0) @binding(2) var<uniform> me: MeP;
var<workgroup> cost: array<atomic<u32>, 17>;

fn cand(i: u32) -> vec2<i32> {
    if (i == 0u) { return vec2(0, 0); }
    let j = i32(i - 1u);
    let mag = j % R + 1;
    let q = j / R;                       // 0:+x 1:-x 2:+y 3:-y
    let s = select(1, -1, (q & 1) == 1);
    return select(vec2(0, s * mag), vec2(s * mag, 0), q < 2);
}
fn magnitude(i: u32) -> i32 {
    let v = cand(i);
    return abs(v.x) + abs(v.y);
}
fn px(l: u32, q: vec2<i32>, dims: vec2<i32>) -> vec3<f32> {
    return textureLoad(frames, clamp(q, vec2(0), dims - 1), l, 0).rgb;
}
fn eq(x: vec3<f32>, y: vec3<f32>) -> bool { return all(abs(x - y) < vec3(0.5 / 255.0)); }
fn changed(q: vec2<i32>, dims: vec2<i32>) -> bool {
    return !eq(px(me.la, q, dims), px(me.lb, q, dims));
}
@compute @workgroup_size(16, 16)
fn tile_me(@builtin(global_invocation_id) gid: vec3<u32>,
           @builtin(local_invocation_index) li: u32,
           @builtin(workgroup_id) wg: vec3<u32>) {
    if (li < NC) { atomicStore(&cost[li], 0u); }
    workgroupBarrier();
    let dims = vec2<i32>(textureDimensions(frames));
    let p = vec2<i32>(gid.xy);
    if (all(p < dims)) {
        let a = px(me.la, p, dims);
        let b = px(me.lb, p, dims);
        if (!eq(a, b)) {
            atomicAdd(&cost[0], 1u);
            for (var i = 1u; i < NC; i++) {
                let v = cand(i);
                var c = 0u;
                if (!eq(a, px(me.lb, p + v, dims)) && !eq(b, px(me.la, p - v, dims))) { c += 1u; }
                if (changed(p + v, dims) || changed(p - v, dims)) { c += 1u; }
                atomicAdd(&cost[i], c);
            }
        }
    }
    workgroupBarrier();
    if (li == 0u) {
        var best = 0u;
        var best_cost = atomicLoad(&cost[0]);
        for (var i = 1u; i < NC; i++) {
            let c = atomicLoad(&cost[i]);
            if (c < best_cost || (c == best_cost && magnitude(i) < magnitude(best))) {
                best = i;
                best_cost = c;
            }
        }
        vecs[wg.y * me.tiles_x + wg.x] = cand(best);
    }
}
