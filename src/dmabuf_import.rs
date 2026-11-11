//! Imports the gamescope core's dmabuf ring into Bevy's Vulkan device as
//! buffers, and copies each frame out of its slot with a compute swizzle into a
//! pooled `Rgba8Unorm` texture. See docs/GAMESCOPE_ZEROCOPY.md.

use std::os::fd::{AsRawFd, BorrowedFd, IntoRawFd};
use std::sync::Arc;

use ash::vk;
use bevy::prelude::*;
use bevy::render::renderer::{RenderDevice, RenderQueue};
use retro_core::backend::GpuFrame;
use retro_core::frame_import::{DmabufSet, FrameImporter, set_frame_importer};
use wgpu::hal::api::Vulkan;

/// What a [`GpuFrame`] from here holds.
pub struct GpuTexture {
    pub texture: wgpu::Texture,
}

/// Free textures kept in the pool past what is in use.
const SPARE: usize = 2;

const SHADER: &str = r#"
struct Params { width: u32, height: u32, stride: u32, offset: u32 }
@group(0) @binding(0) var<storage, read> src: array<u32>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var dst: texture_storage_2d<rgba8unorm, write>;

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.width || id.y >= params.height) {
        return;
    }
    let v = src[params.offset + id.y * params.stride + id.x];
    let c = vec3<u32>((v >> 16u) & 255u, (v >> 8u) & 255u, v & 255u);
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(vec3<f32>(c) / 255.0, 1.0));
}
"#;

pub fn install(device: Option<Res<RenderDevice>>, queue: Option<Res<RenderQueue>>) {
    let (Some(device), Some(queue)) = (device, queue) else {
        return;
    };
    let device = device.wgpu_device().clone();
    let queue: wgpu::Queue = (**queue.0).clone();
    if !supported(&device) {
        info!("dmabuf: no Vulkan dma_buf import, gamescope frames go through the CPU");
        return;
    }
    let pipeline = Arc::new(Pipeline::new(&device));
    set_frame_importer(move || {
        Some(Box::new(Importer {
            device: device.clone(),
            queue: queue.clone(),
            pipeline: Arc::clone(&pipeline),
            slots: Vec::new(),
            size: (0, 0),
            pool: Vec::new(),
        }) as Box<dyn FrameImporter>)
    });
}

fn supported(device: &wgpu::Device) -> bool {
    let Some(hal) = (unsafe { device.as_hal::<Vulkan>() }) else {
        return false;
    };
    let ext = hal.enabled_device_extensions();
    ext.contains(&ash::khr::external_memory_fd::NAME)
        && ext.contains(&ash::ext::external_memory_dma_buf::NAME)
}

struct Pipeline {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
}

impl Pipeline {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("dmabuf_swizzle"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("dmabuf_swizzle"),
            entries: &[
                entry(
                    0,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    1,
                    wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                ),
                entry(
                    2,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("dmabuf_swizzle"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("dmabuf_swizzle"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self { layout, pipeline }
    }
}

struct Slot {
    buffer: wgpu::Buffer,
    params: wgpu::Buffer,
}

struct Importer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: Arc<Pipeline>,
    slots: Vec<Slot>,
    size: (u32, u32),
    pool: Vec<Arc<GpuTexture>>,
}

impl FrameImporter for Importer {
    fn set_buffers(&mut self, bufs: &DmabufSet) -> bool {
        self.slots.clear();
        self.pool.clear();
        self.size = (bufs.width, bufs.height);
        for b in &bufs.buffers {
            if b.stride % 4 != 0 || b.offset % 4 != 0 {
                return false;
            }
            let Some(buffer) = (unsafe { import_buffer(&self.device, b.fd, b.size) }) else {
                self.slots.clear();
                return false;
            };
            let params = [bufs.width, bufs.height, b.stride / 4, b.offset / 4];
            let params = wgpu::util::DeviceExt::create_buffer_init(
                &self.device,
                &wgpu::util::BufferInitDescriptor {
                    label: Some("dmabuf_params"),
                    contents: bytemuck_u32(&params),
                    usage: wgpu::BufferUsages::UNIFORM,
                },
            );
            self.slots.push(Slot { buffer, params });
        }
        true
    }

    fn import(&mut self, slot: usize, done: Box<dyn FnOnce() + Send>) -> Option<GpuFrame> {
        if slot >= self.slots.len() {
            return None;
        }
        let texture = self.texture();
        let slot = &self.slots[slot];
        let (width, height) = self.size;
        let view = texture.texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dmabuf_swizzle"),
            layout: &self.pipeline.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: slot.buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: slot.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        self.queue.submit([encoder.finish()]);
        self.queue.on_submitted_work_done(done);
        Some(texture)
    }
}

impl Importer {
    /// A pooled texture nothing else holds, or a new one.
    fn texture(&mut self) -> Arc<GpuTexture> {
        let mut free = self
            .pool
            .iter()
            .filter(|t| Arc::strong_count(t) == 1)
            .count();
        self.pool.retain(|t| {
            let drop = Arc::strong_count(t) == 1 && free > SPARE + 1;
            free -= drop as usize;
            !drop
        });
        if let Some(t) = self.pool.iter().find(|t| Arc::strong_count(t) == 1) {
            return Arc::clone(t);
        }
        let (width, height) = self.size;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dmabuf_frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let t = Arc::new(GpuTexture { texture });
        self.pool.push(Arc::clone(&t));
        t
    }
}

fn bytemuck_u32(v: &[u32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast(), std::mem::size_of_val(v)) }
}

/// A `VkBuffer` on the dmabuf's memory, owned by wgpu from then on.
unsafe fn import_buffer(device: &wgpu::Device, fd: BorrowedFd, size: u64) -> Option<wgpu::Buffer> {
    let size = size & !3;
    let dmabuf_size = unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_END) };
    if dmabuf_size <= 0 || size == 0 {
        return None;
    }
    let (buffer, memory, alloc_size) = {
        let hal = unsafe { device.as_hal::<Vulkan>() }?;
        let raw = hal.raw_device();
        let fd_ext =
            ash::khr::external_memory_fd::Device::new(hal.shared_instance().raw_instance(), raw);
        let handle_type = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;

        let mut props = vk::MemoryFdPropertiesKHR::default();
        if let Err(e) =
            unsafe { fd_ext.get_memory_fd_properties(handle_type, fd.as_raw_fd(), &mut props) }
        {
            warn!("dmabuf: vkGetMemoryFdPropertiesKHR: {e}");
            return None;
        }
        let mut external = vk::ExternalMemoryBufferCreateInfo::default().handle_types(handle_type);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::STORAGE_BUFFER)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        let buffer = unsafe { raw.create_buffer(&info, None) }.ok()?;
        let req = unsafe { raw.get_buffer_memory_requirements(buffer) };
        let bits = req.memory_type_bits & props.memory_type_bits;
        if bits == 0 || req.size > dmabuf_size as u64 {
            warn!(
                "dmabuf: cannot import ({} bytes needed of {dmabuf_size}, memory types {:#x} & {:#x})",
                req.size, req.memory_type_bits, props.memory_type_bits
            );
            unsafe { raw.destroy_buffer(buffer, None) };
            return None;
        }
        // NVIDIA offers more types than it imports into: only one like the
        // exporter's (gamescope's HOST_CACHED) works, and a failed try may close the fd.
        let types = unsafe {
            hal.shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(hal.raw_physical_device())
        }
        .memory_types;
        let rank = |i: u32| {
            let f = types[i as usize].property_flags;
            (
                !f.contains(vk::MemoryPropertyFlags::HOST_CACHED),
                !f.contains(vk::MemoryPropertyFlags::HOST_VISIBLE),
                i,
            )
        };
        let index = (0..32)
            .filter(|i| bits & (1 << i) != 0)
            .min_by_key(|&i| rank(i))
            .unwrap();
        let Ok(owned) = fd.try_clone_to_owned() else {
            unsafe { raw.destroy_buffer(buffer, None) };
            return None;
        };
        // A successful import takes the fd.
        let raw_fd = owned.into_raw_fd();
        let mut import = vk::ImportMemoryFdInfoKHR::default()
            .handle_type(handle_type)
            .fd(raw_fd);
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(index)
            .push_next(&mut import);
        let memory = match unsafe { raw.allocate_memory(&alloc, None) } {
            Ok(m) => m,
            Err(e) => {
                warn!("dmabuf: vkAllocateMemory (memory type {index}): {e}");
                unsafe {
                    libc::close(raw_fd);
                    raw.destroy_buffer(buffer, None);
                }
                return None;
            }
        };
        if let Err(e) = unsafe { raw.bind_buffer_memory(buffer, memory, 0) } {
            warn!("dmabuf: vkBindBufferMemory: {e}");
            unsafe {
                raw.free_memory(memory, None);
                raw.destroy_buffer(buffer, None);
            }
            return None;
        }
        (buffer, memory, req.size)
    };
    let hal_buffer =
        unsafe { wgpu::hal::vulkan::Buffer::from_raw_managed(buffer, memory, 0, alloc_size) };
    Some(unsafe {
        device.create_buffer_from_hal::<Vulkan>(
            hal_buffer,
            &wgpu::BufferDescriptor {
                label: Some("dmabuf_slot"),
                size,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            },
        )
    })
}
