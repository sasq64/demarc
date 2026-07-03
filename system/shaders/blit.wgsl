// Passthrough composite blit. The CRT/LCD effect is now applied by the
// librashader `.slangp` filter chain, which renders the emulator framebuffer
// into an intermediate texture at display resolution (preserving the source
// aspect ratio). This shader just maps that intermediate into the view target,
// applying the letterbox/pillarbox transform (`uv_scale`/`uv_offset`) computed
// by `compute_uniform` in `post_process.rs`. Border handling (edge-stretch vs.
// black bars) is done by the bound sampler + a scissor, exactly as before.

#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct PostProcessUniform {
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    // Unused now that the effect lives in the librashader chain; kept so the
    // uniform layout stays identical to `PostProcessUniform` in Rust.
    crt_enabled: u32,
}
@group(0) @binding(2) var<uniform> settings: PostProcessUniform;

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mapped_uv = (in.uv - settings.uv_offset) / settings.uv_scale;
    let c = textureSampleLevel(screen_texture, texture_sampler, mapped_uv, 0.0).rgb;
    return vec4<f32>(c, 1.0);
}
