fn cover(uv: vec2f) -> vec2f {
    let img = vec2f(textureDimensions(tex));
    let s = (ui.resolution / img) / max(ui.resolution.x / img.x, ui.resolution.y / img.y);
    return (uv - 0.5) * s + 0.5;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {
    let c = textureSample(tex, samp, cover(in.uv)).rgb;
    return vec4f(c.rbg * 0.1, 1.0);
}
