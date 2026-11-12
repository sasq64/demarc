//! Motion-compensated frame rate conversion on plain wgpu.
//!
//! Display frames map to source frames on a locked [`Cadence`]. A
//! [`Converter`] then estimates motion between the two source frames either side
//! of a display frame and draws the frame between them, in a 6x integer-scaled
//! target so that a phase of one sixth lands on whole pixels.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

/// Integer scale of the interpolated target, and the phases per source frame.
pub const SCALE: u32 = 6;

/// Frames kept on the GPU; a conversion reads two of them.
const LAYERS: usize = 4;

const TILE: (u32, u32) = (16, 8);

/// Motion candidates per tile, as `me.wgsl` has them.
const CANDIDATES: u64 = 17;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One finished source frame of packed RGBA pixels.
pub type Pixels = Arc<Vec<u32>>;

/// Maps display frames to source frames: `num` sources for every `den` displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    k: u64,
    num: u64,
    den: u64,
}

impl Default for Cadence {
    fn default() -> Self {
        Self::new(5, 6)
    }
}

impl Cadence {
    /// A cadence for `num` source frames per `den` display frames, `num <= den`.
    pub fn new(num: u64, den: u64) -> Self {
        let (num, den) = (num.clamp(1, den.max(1)), den.max(1));
        let g = gcd(num, den);
        Self {
            k: 0,
            num: num / g,
            den: den / g,
        }
    }

    /// Source frames shown per display frame.
    pub fn ratio(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// The source frame `a` and the phase `t`, the fraction of the way to
    /// `a + 1`, of the next display frame.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> (u64, f32) {
        let next = self.peek();
        self.k += 1;
        next
    }

    /// What [`Self::next`] would return, for a caller that has to hold back.
    pub fn peek(&self) -> (u64, f32) {
        let pos = self.k * self.num;
        (pos / self.den, (pos % self.den) as f32 / self.den as f32)
    }

    /// The phases of the display frames that fall on source frame `a`.
    pub fn phases(&self, a: u64) -> impl Iterator<Item = f32> + use<> {
        let (num, den) = (self.num, self.den);
        let first = (a * den).div_ceil(num);
        (first..)
            .map(move |k| k * num - a * den)
            .take_while(move |&pos| pos < den)
            .map(move |pos| pos as f32 / den as f32)
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// A phase as the nearest of the [`SCALE`] steps [`Converter::render`] takes.
pub fn sixths(t: f32) -> u32 {
    ((t * SCALE as f32).round() as u32).min(SCALE - 1)
}

/// Whether `device` can run a conversion of a `width`x`height` source.
pub fn supported(device: &wgpu::Device, width: u32, height: u32) -> bool {
    let limits = device.limits();
    limits.max_compute_workgroups_per_dimension > 0
        && limits.max_storage_buffers_per_shader_stage > 0
        && width.max(height) * SCALE <= limits.max_texture_dimension_2d
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MeParams {
    la: u32,
    lb: u32,
    tiles_x: u32,
    tiles_y: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct InterpParams {
    a: [u32; 4],
    b: [u32; 4],
}

/// The compiled passes, shared by every [`Converter`] on a device.
pub struct Pipelines {
    tile_cost: wgpu::ComputePipeline,
    tile_pick: wgpu::ComputePipeline,
    tile_conf: wgpu::ComputePipeline,
    me_layout: wgpu::BindGroupLayout,
    interp: wgpu::RenderPipeline,
    #[cfg(test)]
    interp_paths: wgpu::RenderPipeline,
    interp_layout: wgpu::BindGroupLayout,
    resolve: wgpu::RenderPipeline,
    resolve_layout: wgpu::BindGroupLayout,
}

fn entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    ty: wgpu::BindingType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty,
        count: None,
    }
}

fn texture_binding(view_dimension: wgpu::TextureViewDimension) -> wgpu::BindingType {
    wgpu::BindingType::Texture {
        sample_type: wgpu::TextureSampleType::Float { filterable: false },
        view_dimension,
        multisampled: false,
    }
}

fn buffer_binding(ty: wgpu::BufferBindingType) -> wgpu::BindingType {
    wgpu::BindingType::Buffer {
        ty,
        has_dynamic_offset: false,
        min_binding_size: None,
    }
}

fn layout(
    device: &wgpu::Device,
    label: &str,
    entries: &[wgpu::BindGroupLayoutEntry],
) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries,
    })
}

fn fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    module: &wgpu::ShaderModule,
    entry_point: &str,
    bind_layout: &wgpu::BindGroupLayout,
    constants: &[(&str, f64)],
) -> wgpu::RenderPipeline {
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(bind_layout)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_full"),
            buffers: &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some(entry_point),
            targets: &[Some(FORMAT.into())],
            compilation_options: wgpu::PipelineCompilationOptions {
                constants,
                ..Default::default()
            },
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl Pipelines {
    pub fn new(device: &wgpu::Device) -> Self {
        use wgpu::BufferBindingType::{Storage, Uniform};
        use wgpu::ShaderStages as Stage;
        use wgpu::TextureViewDimension::{D2, D2Array};

        let me_layout = layout(
            device,
            "frameconv me",
            &[
                entry(0, Stage::COMPUTE, texture_binding(D2Array)),
                entry(
                    1,
                    Stage::COMPUTE,
                    buffer_binding(Storage { read_only: false }),
                ),
                entry(2, Stage::COMPUTE, buffer_binding(Uniform)),
                entry(
                    3,
                    Stage::COMPUTE,
                    buffer_binding(Storage { read_only: false }),
                ),
                entry(
                    4,
                    Stage::COMPUTE,
                    buffer_binding(Storage { read_only: false }),
                ),
                entry(
                    5,
                    Stage::COMPUTE,
                    buffer_binding(Storage { read_only: true }),
                ),
            ],
        );
        let interp_layout = layout(
            device,
            "frameconv interp",
            &[
                entry(0, Stage::FRAGMENT, texture_binding(D2Array)),
                entry(
                    1,
                    Stage::FRAGMENT,
                    buffer_binding(Storage { read_only: true }),
                ),
                entry(2, Stage::FRAGMENT, buffer_binding(Uniform)),
                entry(
                    3,
                    Stage::FRAGMENT,
                    buffer_binding(Storage { read_only: true }),
                ),
            ],
        );
        let resolve_layout = layout(
            device,
            "frameconv resolve",
            &[entry(0, Stage::FRAGMENT, texture_binding(D2))],
        );

        let me_module = device.create_shader_module(wgpu::include_wgsl!("me.wgsl"));
        let interp_module = device.create_shader_module(wgpu::include_wgsl!("interp.wgsl"));
        let resolve_module = device.create_shader_module(wgpu::include_wgsl!("resolve.wgsl"));

        let me_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("frameconv me"),
            bind_group_layouts: &[Some(&me_layout)],
            immediate_size: 0,
        });
        let me_pipeline = |entry_point| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry_point),
                layout: Some(&me_pipeline_layout),
                module: &me_module,
                entry_point: Some(entry_point),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let tile_cost = me_pipeline("tile_cost");
        let tile_pick = me_pipeline("tile_pick");
        let tile_conf = me_pipeline("tile_conf");
        let interp = fullscreen_pipeline(
            device,
            "frameconv interp",
            &interp_module,
            "fs_interp",
            &interp_layout,
            &[],
        );
        #[cfg(test)]
        let interp_paths = fullscreen_pipeline(
            device,
            "frameconv paths",
            &interp_module,
            "fs_interp",
            &interp_layout,
            &[("DEBUG", 1.0)],
        );
        let resolve = fullscreen_pipeline(
            device,
            "frameconv resolve",
            &resolve_module,
            "fs_resolve",
            &resolve_layout,
            &[],
        );
        Self {
            tile_cost,
            tile_pick,
            tile_conf,
            me_layout,
            interp,
            #[cfg(test)]
            interp_paths,
            interp_layout,
            resolve,
            resolve_layout,
        }
    }
}

fn target(device: &wgpu::Device, label: &str, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// The conversion state of one view: its frame ring, motion vectors and targets.
pub struct Converter {
    width: u32,
    height: u32,
    ring: wgpu::Texture,
    /// What each ring layer holds. The `Arc`s keep a frame's address from being
    /// reused while it is what tells that layer's contents apart.
    layers: [Option<Pixels>; LAYERS],
    next_layer: usize,
    /// The pair the vectors were estimated for.
    estimated: Option<(usize, usize)>,
    tiles: (u32, u32),
    vectors: wgpu::Buffer,
    /// The vectors of the pair estimated before.
    prev: wgpu::Buffer,
    me_params: wgpu::Buffer,
    interp_params: wgpu::Buffer,
    me_group: wgpu::BindGroup,
    interp_group: wgpu::BindGroup,
    resolve_group: wgpu::BindGroup,
    six: wgpu::Texture,
    six_view: wgpu::TextureView,
    native: wgpu::Texture,
    native_view: wgpu::TextureView,
}

impl Converter {
    pub fn new(device: &wgpu::Device, pipelines: &Pipelines, width: u32, height: u32) -> Self {
        let ring = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frameconv ring"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: LAYERS as u32,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let ring_view = ring.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let tiles = (width.div_ceil(TILE.0), height.div_ceil(TILE.1));
        let vectors_size = (tiles.0 * tiles.1) as u64 * 16;
        let vectors = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frameconv vectors"),
            size: vectors_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let prev = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frameconv prev vectors"),
            size: vectors_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let doubts = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frameconv doubts"),
            size: (tiles.0 * tiles.1) as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let costs = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frameconv costs"),
            size: (tiles.0 * tiles.1) as u64 * CANDIDATES * 2 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let uniform = |label, size| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        };
        let me_params = uniform("frameconv me params", size_of::<MeParams>() as u64);
        let interp_params = uniform("frameconv interp params", size_of::<InterpParams>() as u64);
        let six = target(device, "frameconv 6x", width * SCALE, height * SCALE);
        let six_view = six.create_view(&Default::default());
        let native = target(device, "frameconv native", width, height);
        let native_view = native.create_view(&Default::default());

        let group = |label, layout, resources: &[wgpu::BindingResource]| {
            let entries: Vec<_> = resources
                .iter()
                .enumerate()
                .map(|(i, resource)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: resource.clone(),
                })
                .collect();
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &entries,
            })
        };
        let me_group = group(
            "frameconv me",
            &pipelines.me_layout,
            &[
                wgpu::BindingResource::TextureView(&ring_view),
                vectors.as_entire_binding(),
                me_params.as_entire_binding(),
                costs.as_entire_binding(),
                doubts.as_entire_binding(),
                prev.as_entire_binding(),
            ],
        );
        let interp_group = group(
            "frameconv interp",
            &pipelines.interp_layout,
            &[
                wgpu::BindingResource::TextureView(&ring_view),
                vectors.as_entire_binding(),
                interp_params.as_entire_binding(),
                doubts.as_entire_binding(),
            ],
        );
        let resolve_group = group(
            "frameconv resolve",
            &pipelines.resolve_layout,
            &[wgpu::BindingResource::TextureView(&six_view)],
        );
        Self {
            width,
            height,
            ring,
            layers: Default::default(),
            next_layer: 0,
            estimated: None,
            tiles,
            vectors,
            prev,
            me_params,
            interp_params,
            me_group,
            interp_group,
            resolve_group,
            six,
            six_view,
            native,
            native_view,
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The interpolated target, [`SCALE`] times the source size.
    pub fn six(&self) -> &wgpu::Texture {
        &self.six
    }

    pub fn six_view(&self) -> &wgpu::TextureView {
        &self.six_view
    }

    /// The target box-averaged to the source size, for a filter chain.
    pub fn native(&self) -> &wgpu::Texture {
        &self.native
    }

    pub fn native_view(&self) -> &wgpu::TextureView {
        &self.native_view
    }

    /// The ring layer holding `frame`, uploading it if it is not there yet.
    fn layer(&mut self, queue: &wgpu::Queue, frame: &Pixels) -> usize {
        if let Some(layer) = self
            .layers
            .iter()
            .position(|held| held.as_ref().is_some_and(|held| Arc::ptr_eq(held, frame)))
        {
            return layer;
        }
        let layer = self.next_layer;
        self.next_layer = (layer + 1) % LAYERS;
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.ring,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer as u32,
                },
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(frame),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(self.width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        self.layers[layer] = Some(Arc::clone(frame));
        // Whatever pair the vectors belong to may just have been overwritten.
        if self
            .estimated
            .is_some_and(|(a, b)| a == layer || b == layer)
        {
            self.estimated = None;
        }
        layer
    }

    /// Record the frame at phase `n` sixths from `a` to `b` into the targets.
    ///
    /// The queue's writes land before the next submit, so a converter takes one
    /// call per submit. Both frames must hold `width * height` pixels.
    pub fn render(
        &mut self,
        pipelines: &Pipelines,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        a: &Pixels,
        b: &Pixels,
        n: u32,
    ) {
        let interp = &pipelines.interp;
        self.render_with(interp, pipelines, queue, encoder, a, b, n);
    }

    fn draw(
        &self,
        pipelines: &Pipelines,
        encoder: &mut wgpu::CommandEncoder,
        interp: &wgpu::RenderPipeline,
    ) {
        fullscreen_pass(
            encoder,
            "frameconv interp",
            &self.six_view,
            interp,
            &self.interp_group,
        );
        fullscreen_pass(
            encoder,
            "frameconv resolve",
            &self.native_view,
            &pipelines.resolve,
            &self.resolve_group,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn render_with(
        &mut self,
        interp: &wgpu::RenderPipeline,
        pipelines: &Pipelines,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        a: &Pixels,
        b: &Pixels,
        n: u32,
    ) {
        let la = self.layer(queue, a);
        let lb = self.layer(queue, b);

        if self.estimated != Some((la, lb)) {
            self.estimated = Some((la, lb));
            let params = MeParams {
                la: la as u32,
                lb: lb as u32,
                tiles_x: self.tiles.0,
                tiles_y: self.tiles.1,
            };
            queue.write_buffer(&self.me_params, 0, bytemuck::bytes_of(&params));
            encoder.copy_buffer_to_buffer(&self.vectors, 0, &self.prev, 0, None);
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("frameconv me"),
                timestamp_writes: None,
            });
            pass.set_bind_group(0, &self.me_group, &[]);
            pass.set_pipeline(&pipelines.tile_cost);
            pass.dispatch_workgroups(self.tiles.0, self.tiles.1, 1);
            pass.set_pipeline(&pipelines.tile_pick);
            pass.dispatch_workgroups(self.tiles.0, self.tiles.1, 1);
            pass.set_pipeline(&pipelines.tile_conf);
            pass.dispatch_workgroups(self.tiles.0, self.tiles.1, 1);
        }

        let params = InterpParams {
            a: [la as u32, lb as u32, n, 0],
            b: [self.tiles.0, self.tiles.1, 0, 0],
        };
        queue.write_buffer(&self.interp_params, 0, bytemuck::bytes_of(&params));
        self.draw(pipelines, encoder, interp);
    }
}

fn fullscreen_pass(
    encoder: &mut wgpu::CommandEncoder,
    label: &str,
    target: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.draw(0..3, 0..1);
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
