use bevy::{
    camera::Camera,
    core_pipeline::{
        FullscreenShader,
        core_2d::graph::{Core2d, Node2d},
    },
    ecs::query::QueryItem,
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
        render_graph::{
            NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
        },
        render_resource::{
            AddressMode, BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, PipelineCache,
            RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
            SamplerDescriptor, ShaderStages, ShaderType, TextureFormat, TextureSampleType,
            binding_types::{sampler, texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice},
        settings::WgpuFeatures,
        texture::GpuImage,
        view::ViewTarget,
    },
};
// `SamplerBorderColor` isn't re-exported by Bevy; pull it from wgpu directly.
use wgpu::SamplerBorderColor;

use bevy::asset::{load_internal_asset, uuid_handle};
use bevy::shader::Shader;

use crate::AppSettings;

/// The Lottes CRT shader, embedded into the binary at build time (via
/// `load_internal_asset!`/`include_str!`) so no `shaders/lottes.wgsl` asset file
/// is needed at runtime. The UUID is an arbitrary fixed id for the handle.
const LOTTES_SHADER_HANDLE: Handle<Shader> = uuid_handle!("b1a2c3d4-e5f6-47a8-9bcd-ef0123456789");

pub struct PostProcessPlugin;

impl Plugin for PostProcessPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(
            app,
            LOTTES_SHADER_HANDLE,
            "../system/shaders/lottes.wgsl",
            Shader::from_wgsl
        );

        app.add_plugins((
            ExtractResourcePlugin::<AppSettings>::default(),
            ExtractComponentPlugin::<PostProcess>::default(),
            ExtractComponentPlugin::<PostProcessUniform>::default(),
            UniformComponentPlugin::<PostProcessUniform>::default(),
        ))
        .add_systems(PostUpdate, update_post_process_uniform);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app
            .add_systems(RenderStartup, init_lottes_pipeline)
            .add_render_graph_node::<ViewNodeRunner<PostProcessNode>>(Core2d, PostProcessLabel)
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::Tonemapping,
                    PostProcessLabel,
                    Node2d::EndMainPassPostProcessing,
                ),
            );
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct PostProcessLabel;

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
        let uniform = compute_uniform(pp, &(*settings), camera, &images);
        match existing {
            Some(mut u) => *u = uniform,
            None => {
                commands.entity(entity).insert(uniform);
            }
        }
    }
}

fn compute_uniform(
    pp: &PostProcess,
    settings: &AppSettings,
    camera: &Camera,
    images: &Assets<Image>,
) -> PostProcessUniform {
    let crt_enabled = settings.crt_effect as u32;
    let identity = PostProcessUniform {
        uv_scale: Vec2::ONE,
        uv_offset: Vec2::ZERO,
        crt_enabled,
    };
    if matches!(settings.scale_mode, ScaleMode::Stretch) {
        return identity;
    }
    let Some(target) = camera.physical_target_size() else {
        return identity;
    };
    let Some(source) = images.get(&pp.source) else {
        return identity;
    };
    let src = source.size();
    if target.x == 0 || target.y == 0 || src.x == 0 || src.y == 0 {
        return identity;
    }
    let target_aspect = target.x as f32 / target.y as f32;
    // Use the display aspect ratio the core reports; fall back to the texture's
    // pixel dimensions when the core doesn't supply one.
    let base_aspect = if pp.aspect > 0.0 {
        pp.aspect
    } else {
        src.x as f32 / src.y as f32
    };
    let source_aspect = base_aspect * pp.aspect_tweak;

    let mut sm = settings.scale_mode;

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
    PostProcessUniform {
        uv_scale: scale,
        uv_offset: Vec2::new((1.0 - scale.x) * 0.5, (1.0 - scale.y) * 0.5),
        crt_enabled,
    }
}

#[derive(Default)]
struct PostProcessNode;

impl ViewNode for PostProcessNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static PostProcess,
        &'static DynamicUniformIndex<PostProcessUniform>,
        &'static ExtractedCamera,
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, post_process, uniform_index, camera): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let pipeline_resource = world.resource::<PostProcessPipeline>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let gpu_images = world.resource::<RenderAssets<GpuImage>>();
        let uniforms = world.resource::<ComponentUniforms<PostProcessUniform>>();

        let settings = world.resource::<AppSettings>();

        let Some(pipeline) = pipeline_cache.get_render_pipeline(pipeline_resource.pipeline_id)
        else {
            return Ok(());
        };

        let Some(source_image) = gpu_images.get(&post_process.source) else {
            return Ok(());
        };

        let Some(uniform_binding) = uniforms.uniforms().binding() else {
            return Ok(());
        };

        let sampler = match settings.border_mode {
            BorderMode::Stretch => &pipeline_resource.sampler_stretch,
            BorderMode::Black => &pipeline_resource.sampler_black,
        };

        let bind_group = render_context.render_device().create_bind_group(
            "lottes_bind_group",
            &pipeline_cache.get_bind_group_layout(&pipeline_resource.layout),
            &BindGroupEntries::sequential((
                &source_image.texture_view,
                sampler,
                uniform_binding.clone(),
            )),
        );

        let mut render_pass = render_context.begin_tracked_render_pass(RenderPassDescriptor {
            label: Some("lottes_pass"),
            color_attachments: &[Some(view_target.get_unsampled_color_attachment())],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // Restrict the fullscreen blit to this camera's viewport so grid-mode
        // emulators each draw into their own quadrant instead of overdrawing
        // the whole window. Without a viewport (single-emulator case) this is a
        // no-op and the triangle covers the full target.
        if let Some(viewport) = &camera.viewport {
            render_pass.set_camera_viewport(viewport);
        }

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
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

fn init_lottes_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
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
    let shader = LOTTES_SHADER_HANDLE;

    let pipeline_id = pipeline_cache.queue_render_pipeline(RenderPipelineDescriptor {
        label: Some("lottes_pipeline".into()),
        layout: vec![layout.clone()],
        vertex: fullscreen_shader.to_vertex_state(),
        fragment: Some(FragmentState {
            shader,
            targets: vec![Some(ColorTargetState {
                format: TextureFormat::bevy_default(),
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
