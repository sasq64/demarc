// One vector per 16x8 tile for the source pair A -> B. Only pixels that changed
// are scored: static ones would outvote a thin moving edge on a textured
// background. A changed pixel costs 1 if neither side of it matches along v (one
// side is occluded, so one is enough), and 1 more if v lands it on a pixel changed
// the same way, which is how the speed is found on a uniform background. A rigid
// true vector cannot do that, however close together the strokes are.
//
// tile_cost scores every tile, then tile_pick decides from a tile and its four
// neighbours, as one tile often holds too few strokes to decide alone. Strokes on
// a regular grid can tie a vector with the opposite one or with a multiple of
// itself; a count over all pixels, static or not, breaks those ties. Within one
// direction only a perfect count may do that, as a periodic background would
// otherwise pick a multiple of the true speed.

const R: i32 = 4;
const NC: u32 = 17u;
struct MeP { la: u32, lb: u32, tiles_x: u32, tiles_y: u32 };
@group(0) @binding(0) var frames: texture_2d_array<f32>;
@group(0) @binding(1) var<storage, read_write> vecs: array<vec4<i32>>; // (v, rigid, _)
@group(0) @binding(2) var<uniform> me: MeP;
@group(0) @binding(3) var<storage, read_write> costs: array<u32>;
var<workgroup> cost: array<atomic<u32>, 17>;
var<workgroup> whole: array<atomic<u32>, 17>;
var<workgroup> picked: vec2<i32>;
var<workgroup> miss: atomic<u32>;

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
fn same_change(a: vec3<f32>, b: vec3<f32>, q: vec2<i32>, dims: vec2<i32>) -> bool {
    return eq(a, px(me.la, q, dims)) && eq(b, px(me.lb, q, dims));
}
fn tile_cost_of(t: vec2<i32>, i: u32) -> u32 {
    let c = clamp(t, vec2(0), vec2<i32>(i32(me.tiles_x), i32(me.tiles_y)) - 1);
    return costs[(u32(c.y) * me.tiles_x + u32(c.x)) * NC * 2u + i];
}
fn cross_sum(t: vec2<i32>, i: u32) -> u32 {
    return tile_cost_of(t, i) + tile_cost_of(t + vec2(1, 0), i) + tile_cost_of(t - vec2(1, 0), i)
        + tile_cost_of(t + vec2(0, 1), i) + tile_cost_of(t - vec2(0, 1), i);
}
fn direction(i: u32) -> u32 { return (i + 3u) / u32(R); }
fn pick_vector(t: vec2<i32>) -> vec2<i32> {
    if (tile_cost_of(t, 0u) == 0u) { return vec2(0, 0); }
    var best = 0u;
    var best_cost = 0xffffffffu;
    var best_whole = 0xffffffffu;
    var best_exact = false;
    for (var i = 0u; i < NC; i++) {
        let c = cross_sum(t, i);
        let w = cross_sum(t, i + NC);
        let tied = c == best_cost;
        let exact = w == 0u;
        let closer = direction(i) == direction(best) && magnitude(i) < magnitude(best);
        let fuller = direction(i) != direction(best) && w < best_whole;
        let better = exact != best_exact && exact || exact == best_exact && (closer || fuller);
        if (c < best_cost || (tied && better)) {
            best = i;
            best_cost = c;
            best_whole = w;
            best_exact = exact;
        }
    }
    return cand(best);
}
@compute @workgroup_size(16, 8)
fn tile_cost(@builtin(global_invocation_id) gid: vec3<u32>,
           @builtin(local_invocation_index) li: u32,
           @builtin(workgroup_id) wg: vec3<u32>) {
    if (li < NC) {
        atomicStore(&cost[li], 0u);
        atomicStore(&whole[li], 0u);
    }
    workgroupBarrier();
    let dims = vec2<i32>(textureDimensions(frames));
    let p = vec2<i32>(gid.xy);
    if (all(p < dims)) {
        let a = px(me.la, p, dims);
        let b = px(me.lb, p, dims);
        for (var i = 0u; i < NC; i++) {
            if (!eq(a, px(me.lb, p + cand(i), dims))) { atomicAdd(&whole[i], 1u); }
        }
        if (!eq(a, b)) {
            atomicAdd(&cost[0], 1u);
            for (var i = 1u; i < NC; i++) {
                let v = cand(i);
                var c = 0u;
                if (!eq(a, px(me.lb, p + v, dims)) && !eq(b, px(me.la, p - v, dims))) { c += 1u; }
                if (same_change(a, b, p + v, dims) || same_change(a, b, p - v, dims)) { c += 1u; }
                atomicAdd(&cost[i], c);
            }
        }
    }
    workgroupBarrier();
    if (li == 0u) {
        let tile = (wg.y * me.tiles_x + wg.x) * NC * 2u;
        for (var i = 0u; i < NC; i++) {
            costs[tile + i] = atomicLoad(&cost[i]);
            costs[tile + NC + i] = atomicLoad(&whole[i]);
        }
    }
}
// A tile is rigid when every pixel in it obeys its vector, so it has no static
// background that could fool the interpolation.
@compute @workgroup_size(16, 8)
fn tile_pick(@builtin(global_invocation_id) gid: vec3<u32>,
             @builtin(local_invocation_index) li: u32,
             @builtin(workgroup_id) wg: vec3<u32>) {
    if (li == 0u) {
        atomicStore(&miss, 0u);
        picked = pick_vector(vec2<i32>(wg.xy));
    }
    workgroupBarrier();
    let v = picked;
    let dims = vec2<i32>(textureDimensions(frames));
    let p = vec2<i32>(gid.xy);
    if (all(p < dims) && !eq(px(me.la, p, dims), px(me.lb, p + v, dims))) {
        atomicStore(&miss, 1u);
    }
    workgroupBarrier();
    if (li == 0u) {
        let rigid = atomicLoad(&miss) == 0u && any(v != vec2(0));
        vecs[wg.y * me.tiles_x + wg.x] = vec4(v, select(0, 1, rigid), 0);
    }
}
