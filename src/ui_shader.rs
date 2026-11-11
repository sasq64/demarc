//! Draws the theme's WGSL background shaders ([`retro_ui::ShaderBg`]) as egui
//! paint callbacks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bevy::{
    asset::RenderAssetUsages,
    core_pipeline::FullscreenShader,
    diagnostic::FrameCount,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        render_asset::RenderAssets,
        render_phase::TrackedRenderPass,
        render_resource::{
            AddressMode, BindGroup, BindGroupEntries, BindGroupLayoutDescriptor,
            BindGroupLayoutEntries, BlendState, BufferInitDescriptor, BufferUsages,
            CachedRenderPipelineId, ColorTargetState, ColorWrites, FilterMode, FragmentState,
            PipelineCache, RenderPipelineDescriptor, Sampler, SamplerBindingType,
            SamplerDescriptor, ShaderStages, ShaderType, TextureFormat, TextureSampleType,
            binding_types::{sampler, texture_2d, uniform_buffer},
            encase,
        },
        renderer::RenderDevice,
        sync_world::RenderEntity,
        texture::{FallbackImage, GpuImage},
    },
};
use bevy_egui::{
    EguiContexts, egui,
    render::{EguiBevyPaintCallback, EguiBevyPaintCallbackImpl, EguiPipelineKey},
};
use retro_ui::ShaderBg;

use crate::config::Args;

const PRELUDE: &str = "#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput
struct Ui { resolution: vec2f, origin: vec2f, screen: vec2f, time: f32, _pad: f32 }
@group(0) @binding(0) var<uniform> ui: Ui;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
";

#[derive(ShaderType, Clone, Copy)]
struct UiUniform {
    resolution: Vec2,
    origin: Vec2,
    screen: Vec2,
    time: f32,
    _pad: f32,
}

#[derive(Clone)]
struct Loaded {
    shader: Handle<Shader>,
    image: Option<Handle<Image>>,
}

struct Entry {
    loaded: Loaded,
    mtimes: (Option<SystemTime>, Option<SystemTime>),
}

#[derive(Default)]
struct Shared {
    loaded: HashMap<ShaderBg, Loaded>,
    time: f32,
    screen: Vec2,
}

#[derive(Resource, Default)]
pub(crate) struct UiShaders {
    entries: HashMap<ShaderBg, Entry>,
    shared: Arc<Mutex<Shared>>,
    installed: bool,
    next_check: f32,
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn theme_dir(args: &Args) -> PathBuf {
    args.theme
        .as_deref()
        .map(crate::egui_ui::theme_path)
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| newsys::system_dir().join("themes"))
}

fn load_image(path: &Path) -> Result<Image, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    Ok(Image::from_dynamic(
        img,
        true,
        RenderAssetUsages::RENDER_WORLD,
    ))
}

/// Loads every shader the theme uses, again whenever a file changes, and keeps
/// the egui painter supplied with the handles.
pub(crate) fn update_shaders(
    mut contexts: EguiContexts,
    mut state: ResMut<UiShaders>,
    args: Res<Args>,
    time: Res<Time<Real>>,
    mut shaders: ResMut<Assets<Shader>>,
    mut images: ResMut<Assets<Image>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let state = &mut *state;
    if !state.installed {
        state.installed = true;
        let shared = state.shared.clone();
        retro_ui::set_shader_painter(
            ctx,
            Arc::new(move |rect, bg| {
                let shared = shared.lock().unwrap();
                let loaded = shared.loaded.get(bg)?.clone();
                Some(egui::Shape::Callback(
                    EguiBevyPaintCallback::new_paint_callback(
                        rect,
                        UiShaderCallback {
                            loaded,
                            time: shared.time,
                            screen: shared.screen,
                            prepared: Mutex::new(None),
                        },
                    ),
                ))
            }),
        );
    }
    let now = time.elapsed_secs();
    {
        let mut shared = state.shared.lock().unwrap();
        shared.time = now;
        let size = ctx.content_rect().size() * ctx.pixels_per_point();
        shared.screen = Vec2::new(size.x, size.y);
    }
    if now < state.next_check {
        return Ok(());
    }
    state.next_check = now + 0.5;

    let theme = retro_ui::theme(ctx);
    let dir = theme_dir(&args);
    for bg in [&theme.panel.shader, &theme.screen.shader]
        .into_iter()
        .flatten()
    {
        let shader_path = dir.join(&bg.shader);
        let image_path = bg.texture.as_ref().map(|t| dir.join(t));
        let mtimes = (mtime(&shader_path), image_path.as_deref().and_then(mtime));
        if state.entries.get(bg).is_some_and(|e| e.mtimes == mtimes) {
            continue;
        }
        let source = match std::fs::read_to_string(&shader_path) {
            Ok(src) if src.contains("var<uniform> ui") => src,
            Ok(src) => format!("{PRELUDE}{src}"),
            Err(e) => {
                error!("UI shader {}: {e}", shader_path.display());
                String::new()
            }
        };
        let image = image_path.as_deref().and_then(|p| {
            load_image(p)
                .inspect_err(|e| error!("UI shader texture {}: {e}", p.display()))
                .ok()
        });
        let shader = Shader::from_wgsl(source, shader_path.to_string_lossy().into_owned());
        let entry = state.entries.entry(bg.clone()).or_insert_with(|| Entry {
            loaded: Loaded {
                shader: shaders.reserve_handle(),
                image: None,
            },
            mtimes: (None, None),
        });
        entry.mtimes = mtimes;
        shaders.insert(&entry.loaded.shader, shader)?;
        entry.loaded.image = match (image, entry.loaded.image.take()) {
            (Some(img), Some(handle)) => {
                images.insert(&handle, img)?;
                Some(handle)
            }
            (Some(img), None) => Some(images.add(img)),
            (None, _) => None,
        };
        info!("UI shader {} loaded", shader_path.display());
        state
            .shared
            .lock()
            .unwrap()
            .loaded
            .insert(bg.clone(), entry.loaded.clone());
    }
    Ok(())
}

#[derive(Resource)]
struct UiShaderPipeline {
    layout: BindGroupLayoutDescriptor,
    sampler: Sampler,
    fullscreen: FullscreenShader,
    pipelines: HashMap<(AssetId<Shader>, TextureFormat), CachedRenderPipelineId>,
}

impl UiShaderPipeline {
    fn pipeline(
        &mut self,
        cache: &PipelineCache,
        shader: &Handle<Shader>,
        format: TextureFormat,
    ) -> CachedRenderPipelineId {
        *self
            .pipelines
            .entry((shader.id(), format))
            .or_insert_with(|| {
                cache.queue_render_pipeline(RenderPipelineDescriptor {
                    label: Some("ui_shader".into()),
                    layout: vec![self.layout.clone()],
                    vertex: self.fullscreen.to_vertex_state(),
                    fragment: Some(FragmentState {
                        shader: shader.clone(),
                        targets: vec![Some(ColorTargetState {
                            format,
                            blend: Some(BlendState::ALPHA_BLENDING),
                            write_mask: ColorWrites::ALL,
                        })],
                        ..default()
                    }),
                    ..default()
                })
            })
    }
}

fn init_pipeline(
    mut commands: Commands,
    render_device: Res<RenderDevice>,
    fullscreen: Res<FullscreenShader>,
) {
    let layout = BindGroupLayoutDescriptor::new(
        "ui_shader_layout",
        &BindGroupLayoutEntries::sequential(
            ShaderStages::FRAGMENT,
            (
                uniform_buffer::<UiUniform>(false),
                texture_2d(TextureSampleType::Float { filterable: true }),
                sampler(SamplerBindingType::Filtering),
            ),
        ),
    );
    let sampler = render_device.create_sampler(&SamplerDescriptor {
        address_mode_u: AddressMode::Repeat,
        address_mode_v: AddressMode::Repeat,
        mag_filter: FilterMode::Linear,
        min_filter: FilterMode::Linear,
        ..default()
    });
    commands.insert_resource(UiShaderPipeline {
        layout,
        sampler,
        fullscreen: fullscreen.clone(),
        pipelines: HashMap::new(),
    });
}

/// This frame's bind groups. They live here because a render pass needs them
/// borrowed for the whole pass, longer than the callback that made them.
#[derive(Resource, Default)]
struct BindGroups {
    frame: u32,
    groups: Vec<BindGroup>,
}

struct UiShaderCallback {
    loaded: Loaded,
    time: f32,
    screen: Vec2,
    prepared: Mutex<Option<(CachedRenderPipelineId, usize)>>,
}

impl EguiBevyPaintCallbackImpl for UiShaderCallback {
    fn update(
        &self,
        info: egui::PaintCallbackInfo,
        _render_entity: RenderEntity,
        key: EguiPipelineKey,
        world: &mut World,
    ) {
        let id = world.resource_scope(|world, mut pipeline: Mut<UiShaderPipeline>| {
            pipeline.pipeline(
                world.resource::<PipelineCache>(),
                &self.loaded.shader,
                key.target_format,
            )
        });
        // bevy_egui 0.42 passes a zero screen size here, which clamps
        // `viewport_in_pixels()` to nothing.
        let ppp = info.pixels_per_point;
        let min = (info.viewport.min * ppp).round();
        let max = (info.viewport.max * ppp).round();
        let uniform = UiUniform {
            resolution: Vec2::new(max.x - min.x, max.y - min.y),
            origin: Vec2::new(min.x, min.y),
            screen: self.screen,
            time: self.time,
            _pad: 0.0,
        };
        let mut bytes = encase::UniformBuffer::new(Vec::new());
        if bytes.write(&uniform).is_err() {
            return;
        }
        let device = world.resource::<RenderDevice>();
        let buffer = device.create_buffer_with_data(&BufferInitDescriptor {
            label: Some("ui_shader_uniform"),
            contents: bytes.as_ref(),
            usage: BufferUsages::UNIFORM,
        });
        let pipeline = world.resource::<UiShaderPipeline>();
        let image = self
            .loaded
            .image
            .as_ref()
            .and_then(|h| world.resource::<RenderAssets<GpuImage>>().get(h))
            .unwrap_or(&world.resource::<FallbackImage>().d2);
        let bind_group = device.create_bind_group(
            "ui_shader_bind_group",
            &world
                .resource::<PipelineCache>()
                .get_bind_group_layout(&pipeline.layout),
            &BindGroupEntries::sequential((
                buffer.as_entire_binding(),
                &image.texture_view,
                &pipeline.sampler,
            )),
        );
        let frame = world.resource::<FrameCount>().0;
        let mut groups = world.resource_mut::<BindGroups>();
        if groups.frame != frame {
            groups.frame = frame;
            groups.groups.clear();
        }
        groups.groups.push(bind_group);
        *self.prepared.lock().unwrap() = Some((id, groups.groups.len() - 1));
    }

    fn render<'pass>(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut TrackedRenderPass<'pass>,
        _render_entity: RenderEntity,
        _key: EguiPipelineKey,
        world: &'pass World,
    ) {
        let Some((id, index)) = *self.prepared.lock().unwrap() else {
            return;
        };
        let Some(bind_group) = world.resource::<BindGroups>().groups.get(index) else {
            return;
        };
        let Some(pipeline) = world.resource::<PipelineCache>().get_render_pipeline(id) else {
            return;
        };
        render_pass.set_render_pipeline(pipeline);
        render_pass.set_bind_group(0, bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

pub struct UiShaderPlugin;

impl Plugin for UiShaderPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UiShaders>();
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app
                .init_resource::<BindGroups>()
                .add_systems(RenderStartup, init_pipeline);
        }
    }
}
