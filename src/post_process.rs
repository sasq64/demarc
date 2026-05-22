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
        extract_component::{
            ComponentUniforms, DynamicUniformIndex, ExtractComponent, ExtractComponentPlugin,
            UniformComponentPlugin,
        },
        render_asset::RenderAssets,
        render_graph::{
            NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
        },
        render_resource::{
            BindGroupEntries, BindGroupLayoutDescriptor, BindGroupLayoutEntries,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, FragmentState, PipelineCache,
            RenderPassDescriptor, RenderPipelineDescriptor, Sampler, SamplerBindingType,
            SamplerDescriptor, ShaderStages, ShaderType, TextureFormat, TextureSampleType,
            binding_types::{sampler, texture_2d, uniform_buffer},
        },
        renderer::{RenderContext, RenderDevice},
        texture::GpuImage,
        view::ViewTarget,
    },
};

const LOTTES_SHADER_PATH: &str = "shaders/lottes.wgsl";

pub struct PostProcessPlugin;

impl Plugin for PostProcessPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
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

#[derive(Component, Clone, ExtractComponent)]
pub struct PostProcess {
    pub source: Handle<Image>,
    pub scale_mode: ScaleMode,
}

#[derive(Component, Default, Clone, Copy, ExtractComponent, ShaderType)]
pub struct PostProcessUniform {
    uv_scale: Vec2,
    uv_offset: Vec2,
}

fn update_post_process_uniform(
    images: Res<Assets<Image>>,
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &PostProcess,
        &Camera,
        Option<&mut PostProcessUniform>,
    )>,
) {
    for (entity, pp, camera, existing) in &mut query {
        let uniform = compute_uniform(pp, camera, &images);
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
    camera: &Camera,
    images: &Assets<Image>,
) -> PostProcessUniform {
    let identity = PostProcessUniform {
        uv_scale: Vec2::ONE,
        uv_offset: Vec2::ZERO,
    };
    if matches!(pp.scale_mode, ScaleMode::Stretch) {
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
    let source_aspect = src.x as f32 / src.y as f32;
    let target_wider = target_aspect > source_aspect;
    let scale = match (pp.scale_mode, target_wider) {
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
    }
}

#[derive(Default)]
struct PostProcessNode;

impl ViewNode for PostProcessNode {
    type ViewQuery = (
        &'static ViewTarget,
        &'static PostProcess,
        &'static DynamicUniformIndex<PostProcessUniform>,
    );

    fn run(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext,
        (view_target, post_process, uniform_index): QueryItem<Self::ViewQuery>,
        world: &World,
    ) -> Result<(), NodeRunError> {
        let pipeline_resource = world.resource::<PostProcessPipeline>();
        let pipeline_cache = world.resource::<PipelineCache>();
        let gpu_images = world.resource::<RenderAssets<GpuImage>>();
        let uniforms = world.resource::<ComponentUniforms<PostProcessUniform>>();

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

        let bind_group = render_context.render_device().create_bind_group(
            "lottes_bind_group",
            &pipeline_cache.get_bind_group_layout(&pipeline_resource.layout),
            &BindGroupEntries::sequential((
                &source_image.texture_view,
                &pipeline_resource.sampler,
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

        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, &bind_group, &[uniform_index.index()]);
        render_pass.draw(0..3, 0..1);

        Ok(())
    }
}

#[derive(Resource)]
struct PostProcessPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    pipeline_id: CachedRenderPipelineId,
}

fn init_lottes_pipeline(
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
    let sampler = render_device.create_sampler(&SamplerDescriptor::default());
    let shader = asset_server.load(LOTTES_SHADER_PATH);
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
        sampler,
        pipeline_id,
    });
}
