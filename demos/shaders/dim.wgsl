#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let color = textureSample(screen_texture, texture_sampler, in.uv);
    let color1 = textureSample(screen_texture, texture_sampler, in.uv + vec2<f32>(0.004, 0.004));
    return vec4<f32>(color.rgb * 0.5 * color1.rgb * 0.5, color.a);
}
