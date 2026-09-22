//! The composite pass: every emulator view drawn as one quad into the emulator
//! camera's view target, and the pipelines it draws them with.

use std::collections::HashMap;

use bevy::{
    core_pipeline::FullscreenShader,
    prelude::*,
    render::{
        camera::ExtractedCamera,
        extract_component::{ComponentUniforms, DynamicUniformIndex},
        render_asset::RenderAssets,
        render_resource::{
            AddressMode, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor,
            BindGroupLayoutEntries, BlendComponent, BlendFactor, BlendOperation, BlendState,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, PipelineCache,
            RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
            SamplerDescriptor, ShaderStages, TextureSampleType, TextureView,
            binding_types::{sampler, texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery},
        settings::WgpuFeatures,
        texture::GpuImage,
        view::ViewTarget,
    },
};
// `SamplerBorderColor` isn't re-exported by Bevy; pull it from wgpu directly.
use wgpu::SamplerBorderColor;

use super::chains::{ChainKind, ChainOutput, SlangChains};
use super::geometry::{view_transform, wants_downsample};
use super::{
    BorderMode, BorderScissor, EmuCamera, PostProcess, PostProcessUniform, ShaderEffect,
    ShaderPath, TARGET_FORMAT,
};
use crate::config::RenderSettings;
use crate::dj::{DjCamera, DjView};

/// Draws every emulator view into the one emulator camera's view target.
///
/// Each view is a quad: the fullscreen triangle, mapped to that view's
/// [`ViewRect`](super::ViewRect) with the render pass viewport and clipped to it
/// with the scissor. A grid of `n` emulators is therefore `n` draws in a single
/// render pass, rather than `n` cameras each dragging the whole 2D pipeline
/// (view uniforms, main pass, tonemapping, upscaling) behind them.
///
/// On the [`ShaderEffect::Slangp`] backend a view is two stages: first the
/// librashader `.slangp` filter chain turns the emulator framebuffer into an
/// intermediate texture (the effect, at display resolution, preserving the
/// source aspect ratio), then the composite blit draws that intermediate with
/// the letterbox/pillarbox transform and border handling. Stage 1 is skipped
/// whenever a view wants neither the effect nor the downsampler — the chain
/// would be a verbatim copy — and again on the [`ShaderEffect::Wgsl`] backend,
/// where the single-pass WGSL shader samples the emulator framebuffer directly
/// and applies the effect during the composite itself.
///
/// All of stage 1 runs before the composite pass is opened, because the filter
/// chains record into the same command encoder and a render pass may not be
/// live while they do.
///
/// As a `Core2d` render system this is invoked once per camera with
/// [`CurrentView`](bevy::render::renderer::CurrentView) set; the [`ViewQuery`]
/// skips every camera but the [`EmuCamera`] (notably the UI camera, which draws
/// the HUD on top afterwards).
pub(super) fn post_process_pass(
    view: ViewQuery<(&ViewTarget, &ExtractedCamera, Has<DjCamera>), With<EmuCamera>>,
    views: Query<(
        &PostProcess,
        &PostProcessUniform,
        &DynamicUniformIndex<PostProcessUniform>,
        &BorderScissor,
        Has<DjView>,
    )>,
    mut pipeline_resource: ResMut<PostProcessPipeline>,
    pipeline_cache: Res<PipelineCache>,
    asset_server: Res<AssetServer>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    uniforms: Res<ComponentUniforms<PostProcessUniform>>,
    settings: Res<RenderSettings>,
    shader_path: Res<ShaderPath>,
    mut chains: ResMut<SlangChains>,
    render_queue: Res<RenderQueue>,
    mut render_context: RenderContext,
) {
    let (view_target, camera, dj_camera) = view.into_inner();

    // Both of these are no-ops unless the settings dialog has just changed the
    // shader: the pipeline is already in the map, and the chains already point
    // at this preset.
    let composite_shader = shader_path.effect.composite_shader();
    let opaque_id =
        pipeline_resource.pipeline(&pipeline_cache, &asset_server, composite_shader, false);
    let blended_id =
        pipeline_resource.pipeline(&pipeline_cache, &asset_server, composite_shader, true);
    chains.set_effect(shader_path.effect.slangp());

    // Not compiled yet — the first frames of a run, and of a shader change.
    let (Some(opaque), Some(blended)) = (
        pipeline_cache.get_render_pipeline(opaque_id),
        pipeline_cache.get_render_pipeline(blended_id),
    ) else {
        return;
    };

    let Some(uniform_binding) = uniforms.uniforms().binding() else {
        return;
    };

    let sampler = match settings.border_mode {
        BorderMode::Stretch => &pipeline_resource.sampler_stretch,
        BorderMode::Black => &pipeline_resource.sampler_black,
    };

    // Clip everything to the framebuffer: wgpu rejects a viewport or scissor
    // that pokes outside it, and a stale `ViewRect` (written before a resize
    // this frame) can.
    let framebuffer = camera
        .physical_target_size
        .map(|size| URect::from_corners(UVec2::ZERO, size));

    let mut quads: Vec<Quad> = Vec::with_capacity(views.iter().len());
    'views: for (post_process, uniform, uniform_index, border_scissor, dj_view) in &views {
        // The DJ window draws its one view and nothing else; the main window
        // draws everything but it.
        if dj_view != dj_camera {
            continue;
        }
        let alpha = post_process.alpha.min(1.0);
        if alpha <= 0.0 {
            continue;
        }
        // Inactive (another view is maximized over this one) or not sized yet.
        let Some(mut rect) = post_process.view.rect() else {
            continue;
        };
        if let Some(framebuffer) = framebuffer {
            rect = rect.intersect(framebuffer);
        }
        if rect.is_empty() {
            continue;
        }
        // Restrict the quad to the image rectangle (in `BorderMode::Black`) so
        // the letterbox/pillarbox bars are left showing the camera's clear
        // color rather than being overdrawn by the shader. Without one the
        // scissor is the view rectangle itself — it still has to be set, or the
        // previous quad's scissor would clip this one.
        let scissor = border_scissor.0.map_or(rect, |r| rect.intersect(r));
        if scissor.is_empty() {
            continue;
        }

        let Some(source_image) = gpu_images.get(&post_process.source) else {
            continue;
        };
        // Per-view decision made in `geometry::compute_uniform` (the global
        // `crt_effect` toggle gated by `crt_limit`), not `settings.crt_effect`
        // directly — in grid mode the answer differs from view to view.
        let crt_enabled = uniform.crt_enabled != 0;

        // --- Stage 1 (slangp only): run the librashader chain into an intermediate ---
        //
        // Size the intermediate to the image's actual on-screen pixel extent under
        // the current scale mode, so the effect (scanlines/mask) is rendered at
        // exactly the display pixel density it will be shown at. The composite blit
        // below then samples the intermediate 1:1 (identity texel mapping) instead
        // of bilinearly resampling a fixed-size effect to a different on-screen size
        // — that resampling beats against the high-frequency phosphor mask and
        // produces coloured R/G/B moiré lines (worst under Stretch/Zoom).
        //
        // `inter_size = view rect ⊙ uv_scale`: for Fit it's the aspect-correct image
        // rectangle (unchanged); for Stretch it's the full rect (source stretched to
        // fill); for Zoom it's larger than the rect on the cropped axis — the full
        // scaled image, of which the composite shows the centre. Because the
        // composite's `uv_scale`/`uv_offset` come from `scale_offset` with the same
        // mode and inputs, `(screen_uv - uv_offset) / uv_scale` lands on exact texel
        // centres: one screen pixel per intermediate texel.
        let composite_input: &TextureView = match &shader_path.effect {
            // WGSL backend: the composite shader applies the effect itself while
            // sampling the emulator framebuffer directly — nothing to prepare.
            ShaderEffect::Wgsl(_) => &source_image.texture_view,
            ShaderEffect::Slangp(_) if post_process.raw => &source_image.texture_view,
            ShaderEffect::Slangp(_) => 'slangp: {
                let src_size =
                    UVec2::new(source_image.texture.width(), source_image.texture.height());
                let (image_scale, _) = view_transform(
                    rect.size(),
                    src_size,
                    post_process.used,
                    post_process.aspect,
                    post_process.aspect_tweak,
                    settings.scale_mode,
                );
                // A tiny used area (a demo's 3x3 first window) scales the whole
                // frame far past what a texture may be.
                let max_side = render_context
                    .render_device()
                    .limits()
                    .max_texture_dimension_2d;
                let inter_size = (rect.size().as_vec2() * image_scale)
                    .round()
                    .as_uvec2()
                    .clamp(UVec2::ONE, UVec2::splat(max_side));

                // Minification — the on-screen image is smaller than the source on
                // at least one axis — is the one case the CRT/LCD presets can't
                // help with: there are no spare display pixels for a scanline or a
                // phosphor mask to live in, and squeezing the frame down drops
                // source pixels outright, which aliases. The DREZ preset replaces
                // the effect there; it band-limits the frame as it resamples, so
                // the detail that can't be shown is filtered away instead of
                // beating against the pixel grid. `--downsample` raises that
                // threshold above 1:1 (or, at `0`, switches the preset off
                // entirely). Independent of `crt_enabled`: `crt_limit` has almost
                // always switched the effect off by the time we're down here, and
                // this is a resampler, not a look.
                let downsampling = wants_downsample(inter_size, src_size, chains.downsample_limit);
                // With neither the effect nor the downsampler wanted, the chain
                // this view would run is `stock.slangp` — a nearest-sampled
                // verbatim copy of the source. Composite the emulator framebuffer
                // itself instead: `blit.wgsl` maps uv and linearizes exactly the
                // same way, and skipping the copy saves a full-screen pass, the
                // intermediate texture, and (in grid mode, where `crt_limit`
                // switches most cells off) building any filter chain for this
                // source at all. It is also marginally *more* accurate: the
                // intermediate is `Rgba8UnormSrgb`, so a copy through it encodes
                // and re-decodes every texel, which costs up to 1/255 on bright
                // values. Sampling the `Rgba8Unorm` source is exact.
                let kind = if downsampling {
                    ChainKind::Downsample
                } else if crt_enabled {
                    ChainKind::Effect
                } else {
                    break 'slangp &source_image.texture_view;
                };

                let device = render_context.render_device().clone();
                match chains.render(
                    &device,
                    &render_queue,
                    render_context.command_encoder(),
                    post_process.source.id(),
                    &source_image.texture,
                    inter_size,
                    kind,
                    &shader_path.params,
                ) {
                    ChainOutput::Filtered(view) => view,
                    ChainOutput::Unfiltered => &source_image.texture_view,
                    ChainOutput::Failed => continue 'views,
                }
            }
        };

        // The bind group is built here, while the chain's intermediate is still
        // borrowed, and drawn from below — a `BindGroup` is a cheap handle, a
        // `TextureView` borrow would keep `chains` locked for the whole pass.
        let bind_group = render_context.render_device().create_bind_group(
            "lottes_bind_group",
            &pipeline_cache.get_bind_group_layout(&pipeline_resource.layout),
            &BindGroupEntries::sequential((composite_input, sampler, uniform_binding.clone())),
        );
        quads.push(Quad {
            bind_group,
            uniform_index: uniform_index.index(),
            rect,
            scissor,
            alpha,
        });
    }

    if quads.is_empty() {
        return;
    }

    // A blended view reads what is already in the target, so it has to be drawn
    // over the opaque ones, not under them — that is what makes a cross fade
    // visible whichever entity happens to hold the view.
    quads.sort_by(|a, b| b.alpha.total_cmp(&a.alpha));

    // --- Stage 2: composite every view into the view target ---
    let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("lottes_pass"),
        color_attachments: &[Some(view_target.get_unsampled_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    for quad in &quads {
        if quad.alpha < 1.0 {
            render_pass.set_render_pipeline(blended);
            let a = quad.alpha;
            render_pass.set_blend_constant(LinearRgba::new(a, a, a, a));
        } else {
            render_pass.set_render_pipeline(opaque);
        }
        // The viewport maps the fullscreen triangle onto this view's rectangle
        // (clip-space clipping keeps it there), the scissor trims it to the
        // image when the bars are meant to keep the clear color.
        let size = quad.rect.size().as_vec2();
        let min = quad.rect.min.as_vec2();
        render_pass.set_viewport(min.x, min.y, size.x, size.y, 0.0, 1.0);
        let scissor = quad.scissor.size();
        render_pass.set_scissor_rect(quad.scissor.min.x, quad.scissor.min.y, scissor.x, scissor.y);
        render_pass.set_bind_group(0, &quad.bind_group, &[quad.uniform_index]);
        render_pass.draw(0..3, 0..1);
    }
}

/// One emulator view's composite draw, prepared while the librashader chains
/// are still being recorded and issued once the render pass is open.
struct Quad {
    bind_group: BindGroup,
    /// Offset of this view's `PostProcessUniform` in the dynamic uniform buffer.
    uniform_index: u32,
    /// Where the quad goes, in physical framebuffer pixels.
    rect: URect,
    /// Sub-rect of `rect` the draw is clipped to (see [`BorderScissor`]).
    scissor: URect,
    /// In `(0, 1]`; below `1` the quad is drawn with the blended pipeline.
    alpha: f32,
}

#[derive(Resource)]
pub(super) struct PostProcessPipeline {
    layout: BindGroupLayoutDescriptor,
    /// `ClampToEdge` sampler — used by [`BorderMode::Stretch`].
    sampler_stretch: Sampler,
    /// `ClampToBorder` (black) sampler — used by [`BorderMode::Black`]. Falls
    /// back to a `ClampToEdge` sampler if the adapter lacks the border feature.
    sampler_black: Sampler,
    /// The vertex half of every composite pipeline; only the fragment shader
    /// differs between them.
    fullscreen: FullscreenShader,
    /// Composite pipelines by shader asset path and whether they blend by the
    /// blend constant, queued the first time that shader is selected.
    pipelines: HashMap<(String, bool), CachedRenderPipelineId>,
}

impl PostProcessPipeline {
    /// The composite pipeline that runs `asset_path`, queueing it on first use.
    ///
    /// Lazy rather than queued up front so a run that never opens the settings
    /// dialog — every run, in practice — compiles the one shader it uses.
    fn pipeline(
        &mut self,
        cache: &PipelineCache,
        assets: &AssetServer,
        asset_path: &str,
        blended: bool,
    ) -> CachedRenderPipelineId {
        let key = (asset_path.to_owned(), blended);
        if let Some(id) = self.pipelines.get(&key) {
            return *id;
        }
        // The blend constant carries the view's alpha, so any composite shader
        // (and whatever a filter chain rendered) fades without knowing about it.
        let fade = BlendComponent {
            src_factor: BlendFactor::Constant,
            dst_factor: BlendFactor::OneMinusConstant,
            operation: BlendOperation::Add,
        };
        let id = cache.queue_render_pipeline(RenderPipelineDescriptor {
            label: Some(format!("composite:{asset_path}:{blended}").into()),
            layout: vec![self.layout.clone()],
            vertex: self.fullscreen.to_vertex_state(),
            fragment: Some(FragmentState {
                shader: assets.load(asset_path.to_owned()),
                targets: vec![Some(ColorTargetState {
                    // Matches the view target's main texture format (Bevy's former
                    // `TextureFormat::bevy_default()`, now deprecated).
                    format: TARGET_FORMAT,
                    blend: blended.then_some(BlendState {
                        color: fade,
                        alpha: fade,
                    }),
                    write_mask: ColorWrites::ALL,
                })],
                ..default()
            }),
            ..default()
        });
        self.pipelines.insert(key, id);
        id
    }
}

pub(super) fn init_blit_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    fullscreen_shader: Res<FullscreenShader>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "lottes_bind_group_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
                uniform_buffer::<PostProcessUniform>(true),
            ),
        ),
    );
    let sampler_stretch = render_device.create_sampler(&SamplerDescriptor::default());
    // A black border requires `ClampToBorder`, which is a native-only wgpu
    // feature. Bevy's default `Functionality` priority enables every feature the
    // adapter supports, so this is available on desktop backends — but guard it
    // so an adapter without it falls back to edge-clamping instead of panicking.
    let sampler_black = if render_device
        .features()
        .contains(WgpuFeatures::ADDRESS_MODE_CLAMP_TO_BORDER)
    {
        render_device.create_sampler(&SamplerDescriptor {
            address_mode_u: AddressMode::ClampToBorder,
            address_mode_v: AddressMode::ClampToBorder,
            address_mode_w: AddressMode::ClampToBorder,
            border_color: Some(SamplerBorderColor::OpaqueBlack),
            ..default()
        })
    } else {
        warn!(
            "ADDRESS_MODE_CLAMP_TO_BORDER unsupported; BorderMode::Black will behave like Stretch"
        );
        render_device.create_sampler(&SamplerDescriptor::default())
    };
    // The composite pipeline itself is queued on demand: which shader it runs
    // (the passthrough blit behind a filter chain, or a single-pass effect)
    // depends on the backend in force, which the settings dialog can change.
    // See [`PostProcessPipeline::pipeline`].
    commands.insert_resource(PostProcessPipeline {
        layout,
        sampler_stretch,
        sampler_black,
        fullscreen: fullscreen_shader.clone(),
        pipelines: HashMap::new(),
    });
}
