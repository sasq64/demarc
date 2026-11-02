// Box-averages the 6x target back to native size. Filter chains take their source
// size from the texture, so this is what they read.

const S: i32 = 6;
@group(0) @binding(0) var six: texture_2d<f32>;

@vertex
fn vs_full(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(vec2<u32>((i << 1u) & 2u, i & 2u));
    return vec4(uv * 2.0 - 1.0, 0.0, 1.0);
}
@fragment
fn fs_resolve(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let origin = vec2<i32>(pos.xy) * S;
    var sum = vec3(0.0);
    for (var y = 0; y < S; y++) {
        for (var x = 0; x < S; x++) {
            sum += textureLoad(six, origin + vec2(x, y), 0).rgb;
        }
    }
    return vec4(sum / f32(S * S), 1.0);
}
