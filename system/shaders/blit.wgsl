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

// Convert an sRGB/display value to linear. The librashader `.slangp` chains (and
// the passthrough) output display-ready, gamma-encoded color — that's what a
// RetroArch backbuffer receives and shows directly. Bevy's view target is an
// sRGB texture, so writing those display values straight in would make the
// hardware sRGB-encode them a *second* time, washing the image out and (for
// crt-royale) inflating the subtle diffusion glow into a fat bright outline.
// Linearizing here cancels the view target's encode so the shader's output
// reaches the screen unchanged.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lower = c / 12.92;
    let higher = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(higher, lower, c <= vec3<f32>(0.04045));
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let mapped_uv = (in.uv - settings.uv_offset) / settings.uv_scale;
    let c = textureSampleLevel(screen_texture, texture_sampler, mapped_uv, 0.0).rgb;
    return vec4<f32>(srgb_to_linear(c), 1.0);
}
