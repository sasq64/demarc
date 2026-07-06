use std::collections::HashMap;
use std::path::PathBuf;

use bevy::{
    asset::AssetId,
    camera::Camera,
    core_pipeline::{Core2d, Core2dSystems, FullscreenShader},
    image::Image,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        camera::ExtractedCamera,
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        extract_resource::ExtractResourcePlugin,
        render_asset::RenderAssets,
        render_resource::{
            AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, Extent3d, FragmentState,
            PipelineCache, RenderPassDescriptor, RenderPipelineDescriptor, Sampler,
            SamplerBindingType, SamplerDescriptor, ShaderStages, ShaderType, Texture,
            TextureDescriptor, TextureDimension, TextureFormat, TextureSampleType, TextureUsages,
            TextureView, TextureViewDescriptor,
            binding_types::{sampler, texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice, RenderQueue, ViewQuery},
        settings::WgpuFeatures,
        texture::GpuImage,
        view::ViewTarget,
    },
};
use librashader::presets::ShaderFeatures;
use librashader::runtime::wgpu::{FilterChain, WgpuOutputView};
use librashader::runtime::{Size, Viewport};
// `SamplerBorderColor` isn't re-exported by Bevy; pull it from wgpu directly.
use wgpu::SamplerBorderColor;

use crate::AppSettings;

/// Format of both the librashader intermediate target and the composite blit's
/// output. Matches Bevy's view-target main texture (formerly
/// `TextureFormat::bevy_default()`).
const TARGET_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

/// Filesystem paths of the two `.slangp` presets loaded at runtime: the visible
/// effect (selected on the command line) and a plain passthrough (`stock.slangp`)
/// used when the CRT/LCD effect is toggled off. Absolute paths, resolved from
/// the `system` dir (or a user-supplied `--preset`) in `main`.
#[derive(Resource, Clone)]
pub struct ShaderPath {
    pub effect: PathBuf,
    pub passthrough: PathBuf,
}

pub struct PostProcessPlugin {
    /// Absolute path of the effect `.slangp` preset to run.
    pub effect_path: PathBuf,
    /// Absolute path of the passthrough `.slangp` preset (`stock.slangp`).
    pub passthrough_path: PathBuf,
}

impl Plugin for PostProcessPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractResourcePlugin::<AppSettings>::default(),
            ExtractComponentPlugin::<PostProcess>::default(),
            ExtractComponentPlugin::<PostProcessUniform>::default(),
            ExtractComponentPlugin::<BorderScissor>::default(),
            UniformComponentPlugin::<PostProcessUniform>::default(),
        ))
        .add_systems(PostUpdate, update_post_process_uniform);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Hand the chosen preset paths to the render world so init can load them.
        // A plain resource (not extracted) because they never change.
        render_app.insert_resource(ShaderPath {
            effect: self.effect_path.clone(),
            passthrough: self.passthrough_path.clone(),
        });

        // Bevy 0.19 replaced the render graph with schedule-driven rendering: a
        // render pass is just a system in the per-camera `Core2d` schedule. We run
        // in the `PostProcess` set (where tonemapping lives), which is ordered
        // before upscaling presents the view target to the swapchain.
        render_app
            .add_systems(RenderStartup, (init_blit_pipeline, init_filter_chains))
            .add_systems(Core2d, post_process_pass.in_set(Core2dSystems::PostProcess));
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScaleMode {
    /// Fill the window, distorting the source aspect ratio.
    Stretch,
    /// Preserve the source aspect ratio with letterbox/pillarbox bars.
    #[default]
    Fit,
    /// Preserve the source aspect ratio by scaling to fill the window and
    /// cropping the overflow (top/bottom or left/right).
    Zoom,
}

/// How the shader samples outside the source image (in the letterbox/pillarbox
/// bars, or anywhere `warp` pushes a fetch off the edge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderMode {
    /// `ClampToEdge`: replicate the nearest edge texel, so edge pixels stretch
    /// outward into the border.
    Stretch,
    /// `ClampToBorder` with a black border color: sample black outside the
    /// image, giving a clean black border.
    #[default]
    Black,
}

#[derive(Component, Clone, ExtractComponent)]
pub struct PostProcess {
    pub source: Handle<Image>,
    //pub scale_mode: ScaleMode,
    /// Display aspect ratio (width / height) the core wants the frame shown at.
    /// When `<= 0`, the source texture's pixel dimensions are used instead.
    pub aspect: f32,
    /// Manual multiplier applied on top of `aspect` for fine correction (1.0 = none).
    pub aspect_tweak: f32,
    // How the border (outside the source image) is sampled.
    // pub border_mode: BorderMode,
}

#[derive(Component, Default, Clone, Copy, ExtractComponent, ShaderType)]
pub struct PostProcessUniform {
    uv_scale: Vec2,
    uv_offset: Vec2,
    /// `1` when the CRT effect is active, `0` for a plain passthrough blit.
    crt_enabled: u32,
}

/// Pixel rectangle (in physical framebuffer coords) that the post-process blit
/// is restricted to with a scissor, so the letterbox/pillarbox bars are left
/// untouched and show the camera's clear color instead of being overdrawn.
///
/// `None` means "no clipping" — the blit covers the whole viewport. That's used
/// for [`BorderMode::Stretch`], where the bars are intentionally filled with
/// stretched edge texels by the shader rather than the clear color.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct BorderScissor(pub Option<URect>);

fn update_post_process_uniform(
    images: Res<Assets<Image>>,
    settings: Res<AppSettings>,
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &PostProcess,
        &Camera,
        Option<&mut PostProcessUniform>,
    )>,
) {
    for (entity, pp, camera, existing) in &mut query {
        let uniform = compute_uniform(pp, &settings, camera, &images);
        let scissor = BorderScissor(compute_scissor(&uniform, &settings, camera));
        match existing {
            Some(mut u) => *u = uniform,
            None => {
                commands.entity(entity).insert(uniform);
            }
        }
        commands.entity(entity).insert(scissor);
    }
}

/// Clip rectangle for the blit so the bars keep the clear color. Returns `None`
/// (no clipping) for [`BorderMode::Stretch`], or when the image fills the whole
/// viewport (`Stretch`/`Zoom` scaling), so we never scissor away visible pixels.
fn compute_scissor(
    u: &PostProcessUniform,
    settings: &AppSettings,
    camera: &Camera,
) -> Option<URect> {
    if !matches!(settings.border_mode, BorderMode::Black) {
        return None;
    }
    let rect = camera.physical_viewport_rect()?;
    let vp_min = rect.min.as_vec2();
    let vp_size = rect.size().as_vec2();
    // The image occupies screen-uv `[uv_offset, uv_offset + uv_scale]` within the
    // viewport; the bars are whatever falls outside that. Clamp to the viewport so
    // Zoom/Stretch (which push the image past the edges) just yield the full rect.
    let img_min = (vp_min + u.uv_offset * vp_size).max(vp_min);
    let img_max = (vp_min + (u.uv_offset + u.uv_scale) * vp_size).min(vp_min + vp_size);
    if img_max.x <= img_min.x || img_max.y <= img_min.y {
        return None;
    }
    Some(URect::from_corners(
        img_min.round().as_uvec2(),
        img_max.round().as_uvec2(),
    ))
}

fn compute_uniform(
    pp: &PostProcess,
    settings: &AppSettings,
    camera: &Camera,
    images: &Assets<Image>,
) -> PostProcessUniform {
    // Use the viewport size, not the full render target: in grid mode each
    // camera renders into its own sub-rect, so aspect must be computed against
    // that quadrant. `physical_viewport_size` falls back to the full target
    // when no viewport is set (single-emulator case).
    let (uv_scale, uv_offset) = match (camera.physical_viewport_size(), images.get(&pp.source)) {
        (Some(target), Some(source)) => scale_offset(
            target,
            source.size(),
            pp.aspect,
            pp.aspect_tweak,
            settings.scale_mode,
        ),
        _ => (Vec2::ONE, Vec2::ZERO),
    };
    PostProcessUniform {
        uv_scale,
        uv_offset,
        crt_enabled: settings.crt_effect as u32,
    }
}

/// The letterbox/pillarbox transform for showing a `src`-sized source in a
/// `target`-sized viewport under `scale_mode`. The shader (and the pointer
/// mapping in `retro.rs`) map a screen-uv to the source with
/// `(screen_uv - uv_offset) / uv_scale`; this returns `(uv_scale, uv_offset)`.
/// `Stretch`, a degenerate size, or a target that already matches the source
/// aspect all yield the identity transform.
pub fn scale_offset(
    target: UVec2,
    src: UVec2,
    aspect: f32,
    aspect_tweak: f32,
    scale_mode: ScaleMode,
) -> (Vec2, Vec2) {
    if matches!(scale_mode, ScaleMode::Stretch)
        || target.x == 0
        || target.y == 0
        || src.x == 0
        || src.y == 0
    {
        return (Vec2::ONE, Vec2::ZERO);
    }
    let target_aspect = target.x as f32 / target.y as f32;
    // Use the display aspect ratio the core reports; fall back to the texture's
    // pixel dimensions when the core doesn't supply one.
    let base_aspect = if aspect > 0.0 {
        aspect
    } else {
        src.x as f32 / src.y as f32
    };
    let source_aspect = base_aspect * aspect_tweak;

    let mut sm = scale_mode;
    if (target_aspect - source_aspect).abs() < 0.02 {
        sm = ScaleMode::Zoom;
    }

    let target_wider = target_aspect > source_aspect;
    let scale = match (sm, target_wider) {
        (ScaleMode::Stretch, _) => Vec2::ONE,
        // Fit: shrink the source uv range on the constrained axis, leaving bars.
        (ScaleMode::Fit, true) => Vec2::new(source_aspect / target_aspect, 1.0),
        (ScaleMode::Fit, false) => Vec2::new(1.0, target_aspect / source_aspect),
        // Zoom: expand the screen-uv-to-source-uv ratio on the cropped axis,
        // so screen uv [0,1] maps to a sub-range of the source.
        (ScaleMode::Zoom, true) => Vec2::new(1.0, target_aspect / source_aspect),
        (ScaleMode::Zoom, false) => Vec2::new(source_aspect / target_aspect, 1.0),
    };
    (
        scale,
        Vec2::new((1.0 - scale.x) * 0.5, (1.0 - scale.y) * 0.5),
    )
}

/// Renders the current view in two stages: first the librashader `.slangp`
/// filter chain turns the emulator framebuffer into an intermediate texture
/// (the effect, at display resolution, preserving the source aspect ratio),
/// then a passthrough blit composites that intermediate into the view target
/// with the letterbox/pillarbox transform and border handling.
///
/// As a `Core2d` render system this is invoked once per camera with
/// [`CurrentView`](bevy::render::renderer::CurrentView) set; the [`ViewQuery`]
/// skips cameras without a [`PostProcess`] component.
fn post_process_pass(
    view: ViewQuery<(
        &ViewTarget,
        &PostProcess,
        &DynamicUniformIndex<PostProcessUniform>,
        &ExtractedCamera,
        &BorderScissor,
    )>,
    pipeline_resource: Res<PostProcessPipeline>,
    pipeline_cache: Res<PipelineCache>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    uniforms: Res<ComponentUniforms<PostProcessUniform>>,
    settings: Res<AppSettings>,
    chains: Option<ResMut<SlangChains>>,
    mut render_context: RenderContext,
) {
    let (view_target, post_process, uniform_index, camera, border_scissor) = view.into_inner();

    let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_resource.pipeline_id) else {
        return;
    };

    // Absent if the preset failed to load; skip rendering rather than panic.
    let Some(mut chains) = chains else {
        return;
    };

    let Some(source_image) = gpu_images.get(&post_process.source) else {
        return;
    };

    let Some(uniform_binding) = uniforms.uniforms().binding() else {
        return;
    };

    // --- Stage 1: run the librashader filter chain into an intermediate ---
    //
    // Size the intermediate to the image's actual on-screen pixel extent under
    // the current scale mode, so the effect (scanlines/mask) is rendered at
    // exactly the display pixel density it will be shown at. The composite blit
    // below then samples the intermediate 1:1 (identity texel mapping) instead
    // of bilinearly resampling a fixed-size effect to a different on-screen size
    // — that resampling beats against the high-frequency phosphor mask and
    // produces coloured R/G/B moiré lines (worst under Stretch/Zoom).
    //
    // `inter_size = viewport ⊙ uv_scale`: for Fit it's the aspect-correct image
    // rectangle (unchanged); for Stretch it's the full viewport (source
    // stretched to fill); for Zoom it's larger than the viewport on the cropped
    // axis — the full scaled image, of which the composite shows the centre.
    // Because the composite's `uv_scale`/`uv_offset` come from `scale_offset`
    // with the same mode and inputs, `(screen_uv - uv_offset) / uv_scale` lands
    // on exact texel centres: one screen pixel per intermediate texel.
    let source_id = post_process.source.id();
    let src_size = UVec2::new(source_image.texture.width(), source_image.texture.height());
    let viewport_size = camera
        .physical_viewport_size
        .or(camera.physical_target_size)
        .unwrap_or(src_size);
    let (image_scale, _) = scale_offset(
        viewport_size,
        src_size,
        post_process.aspect,
        post_process.aspect_tweak,
        settings.scale_mode,
    );
    let inter_size = (viewport_size.as_vec2() * image_scale)
        .round()
        .as_uvec2()
        .max(UVec2::ONE);

    let device = render_context.render_device().clone();
    chains.target(&device, source_id, inter_size);
    // Split the borrow so the target (in `targets`) and the chosen chain
    // (`effect`/`passthrough`) can be borrowed at once — they're disjoint fields.
    let SlangChains {
        effect,
        passthrough,
        frame_count,
        targets,
    } = &mut *chains;
    let target = &targets[&source_id];
    let chain = if settings.crt_effect {
        effect
    } else {
        passthrough
    };
    // crt-royale tiles its phosphor mask at a fixed triad size (default 3px =>
    // 24px tiles). When render_width / tile_size is an even integer, a tile
    // boundary lands exactly on the screen center, where the mask's manual
    // frac() tiling has a coordinate discontinuity that duplicates a subpixel
    // (a visible red vertical line). crt-royale's own fix (FIX_DISCONTINUITIES)
    // uses ddx/ddy in a vertex-shared header and won't compile on the
    // slang/glslang path. Instead, nudge the triad size a fraction so the
    // center falls as far as possible from any tile boundary: pick the integer
    // tile size near 24px whose center offset is furthest from a boundary. This
    // keeps triads ~3px (visually identical, mask stays pixel-sharp) and moves
    // the seam off-center. No-op on presets without this parameter.
    if settings.crt_effect {
        use librashader::runtime::FilterChainParameters;
        let width = inter_size.x as f32;
        let mut best_tile = 24i32;
        let mut best_dist = -1.0f32;
        for tile in 22..=26 {
            let center_coord = width / (2.0 * tile as f32);
            let dist = (center_coord - center_coord.round()).abs(); // 0..=0.5
            if dist > best_dist {
                best_dist = dist;
                best_tile = tile;
            }
        }
        chain
            .parameters()
            .set_parameter_value("mask_triad_size_desired", best_tile as f32 / 8.0);
    }
    let lr_size = Size::new(inter_size.x, inter_size.y);
    let output = WgpuOutputView::new_from_raw(&target.view, lr_size, TARGET_FORMAT);
    let viewport = Viewport {
        x: 0.0,
        y: 0.0,
        mvp: None,
        output,
        size: lr_size,
    };
    if let Err(err) = chain.frame(
        &source_image.texture,
        &viewport,
        render_context.command_encoder(),
        *frame_count,
        None,
    ) {
        error!("librashader frame failed: {err}");
        return;
    }
    *frame_count += 1;

    // --- Stage 2: composite the intermediate into the view target ---
    let sampler = match settings.border_mode {
        BorderMode::Stretch => &pipeline_resource.sampler_stretch,
        BorderMode::Black => &pipeline_resource.sampler_black,
    };

    let bind_group = render_context.render_device().create_bind_group(
        "lottes_bind_group",
        &pipeline_cache.get_bind_group_layout(&pipeline_resource.layout),
        &BindGroupEntries::sequential((&target.view, sampler, uniform_binding.clone())),
    );

    let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
        label: Some("lottes_pass"),
        color_attachments: &[Some(view_target.get_unsampled_color_attachment())],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });

    // Restrict the fullscreen blit to this camera's viewport so grid-mode
    // emulators each draw into their own quadrant instead of overdrawing
    // the whole window. Without a viewport (single-emulator case) this is a
    // no-op and the triangle covers the full target.
    if let Some(viewport) = &camera.viewport {
        render_pass.set_camera_viewport(viewport);
    }

    // Restrict the blit to the image rectangle (in `BorderMode::Black`) so the
    // letterbox/pillarbox bars are left showing the camera's clear color rather
    // than being overdrawn by the shader.
    if let (Some(rect), Some(target)) = (border_scissor.0, camera.physical_target_size) {
        let max = rect.max.min(target);
        let min = rect.min.min(max);
        if max.x > min.x && max.y > min.y {
            render_pass.set_scissor_rect(min.x, min.y, max.x - min.x, max.y - min.y);
        }
    }

    render_pass.set_render_pipeline(pipeline);
    render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
    render_pass.draw(0..3, 0..1);
}

#[derive(Resource)]
struct PostProcessPipeline {
    layout: BindGroupLayoutDescriptor,
    /// `ClampToEdge` sampler — used by [`BorderMode::Stretch`].
    sampler_stretch: Sampler,
    /// `ClampToBorder` (black) sampler — used by [`BorderMode::Black`]. Falls
    /// back to a `ClampToEdge` sampler if the adapter lacks the border feature.
    sampler_black: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

/// A librashader intermediate render target: the emulator framebuffer with the
/// filter chain applied, at display resolution and preserving the source aspect
/// ratio. Sized to the [`ScaleMode::Fit`] image rectangle of the view; recreated
/// when that size changes (window resize / core resolution change).
struct IntermediateTarget {
    size: UVec2,
    texture: Texture,
    view: TextureView,
}

/// The two librashader filter chains plus per-emulator intermediate targets.
///
/// `FilterChainWgpu` owns clones of the wgpu `Device`/`Queue` and is `Send`/`Sync`,
/// so it lives as a render-world resource. `frame()` takes `&mut self`, and the
/// targets are recreated on resize, so the whole thing is accessed via `ResMut`.
#[derive(Resource)]
struct SlangChains {
    /// The visible effect preset (CRT/LCD), selected on the command line.
    effect: FilterChain,
    /// Plain passthrough (`stock.slangp`), used when the effect is toggled off.
    passthrough: FilterChain,
    /// RetroArch-style frame counter fed to the shaders (feedback/animation).
    frame_count: usize,
    /// One intermediate target per emulator, keyed by its source image.
    targets: HashMap<AssetId<Image>, IntermediateTarget>,
}

impl SlangChains {
    /// Return the intermediate target for `source`, creating or resizing it to
    /// `size` first. `size` is the display-resolution image rectangle.
    fn target(
        &mut self,
        device: &RenderDevice,
        source: AssetId<Image>,
        size: UVec2,
    ) -> &IntermediateTarget {
        let entry = self.targets.entry(source);
        let needs_build = !matches!(entry, std::collections::hash_map::Entry::Occupied(ref e) if e.get().size == size);
        if needs_build {
            let texture = device.create_texture(&TextureDescriptor {
                label: Some("slang_intermediate"),
                size: Extent3d {
                    width: size.x,
                    height: size.y,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: TextureDimension::D2,
                format: TARGET_FORMAT,
                // librashader renders into it; the composite blit samples it.
                usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&TextureViewDescriptor::default());
            self.targets.insert(
                source,
                IntermediateTarget {
                    size,
                    texture,
                    view,
                },
            );
        }
        &self.targets[&source]
    }
}

/// Load both `.slangp` filter chains onto Bevy's own wgpu device/queue, so they
/// share the emulator's source textures and the view target directly.
fn init_filter_chains(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    render_queue: Res<RenderQueue>,
    shader_path: Res<ShaderPath>,
) {
    let device = render_device.wgpu_device();
    let queue: &wgpu::Queue = &render_queue;
    let load = |path: &PathBuf| {
        FilterChain::load_from_path(path, ShaderFeatures::NONE, device, queue, None)
    };
    let effect = match load(&shader_path.effect) {
        Ok(chain) => chain,
        Err(err) => {
            error!(
                "failed to load shader preset {:?}: {err}",
                shader_path.effect
            );
            return;
        }
    };
    let passthrough = match load(&shader_path.passthrough) {
        Ok(chain) => chain,
        Err(err) => {
            error!(
                "failed to load passthrough preset {:?}: {err}",
                shader_path.passthrough
            );
            return;
        }
    };
    commands.insert_resource(SlangChains {
        effect,
        passthrough,
        frame_count: 0,
        targets: HashMap::new(),
    });
}

fn init_blit_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    asset_server: Res<AssetServer>,
    fullscreen_shader: Res<FullscreenShader>,
    pipeline_cache: Res<PipelineCache>,
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
    // Passthrough composite blit; the CRT/LCD effect is applied upstream by the
    // librashader filter chain into an intermediate texture that this samples.
    let shader = asset_server.load("shaders/blit.wgsl");

    let pipeline_id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("lottes_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader,
            targets: vec![Some(ColorTargetState {
                // Matches the view target's main texture format (Bevy's former
                // `TextureFormat::bevy_default()`, now deprecated).
                format: TARGET_FORMAT,
                blend: None,
                write_mask: ColorWrites::ALL,
            })],
            ..default()
        }),
        ..default()
    });
    commands.insert_resource(PostProcessPipeline {
        layout,
        sampler_stretch,
        sampler_black,
        pipeline_id,
    });
}
