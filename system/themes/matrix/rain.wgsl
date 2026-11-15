fn cover(uv: vec2f) -> vec2f {
    let img = vec2f(textureDimensions(tex));
    let s = (ui.resolution / img) / max(ui.resolution.x / img.x, ui.resolution.y / img.y);
    return (uv - 0.5) * s + 0.5;
}

fn hash(n: f32) -> f32 {
    return fract(sin(n * 127.1) * 43758.5453);
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {
    let c = textureSample(tex, samp, cover(in.uv)).rgb;
    let col = floor(in.uv.x * 96.0);
    let head = fract(ui.time * (0.08 + 0.12 * hash(col)) + hash(col + 7.0));
    let d = head - in.uv.y;
    let trail = select(0.0, exp(-d * 6.0), d >= 0.0);
    return vec4f(c * (0.55 + 0.9 * trail), 1.0);
}
