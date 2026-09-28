//! `c64` — a minimal C64 player: one 720x576 window, the VICE libretro core,
//! and the lottes CRT shader on top.
//!
//! No Bevy, no librashader, no CLI: winit + wgpu only, which is the stack an
//! Android port needs — see `docs/ANDROID.md`.

#![allow(dead_code)]

use crate::libretro;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tracing::info;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::backend::frame_bytes;
use crate::retro_emu::RetroCoreDirect;
use crate::{dylib_name, find_core, system_dir};

const CORE_NAME: &str = "vice_x64sc";
const WINDOW_SIZE: (u32, u32) = (720, 576);

// -------------------------------------------------------------------------
// Keyboard
// -------------------------------------------------------------------------

/// winit physical key → libretro keycode, for the keys a C64 has (or that VICE
/// maps onto one).
fn retro_key(key: KeyCode) -> Option<libretro::retro_key> {
    use KeyCode::*;
    use libretro::*;
    Some(match key {
        Backspace => RETROK_BACKSPACE,
        Tab => RETROK_TAB,
        Enter | NumpadEnter => RETROK_RETURN,
        Escape => RETROK_ESCAPE,
        Space => RETROK_SPACE,
        Quote => RETROK_QUOTE,
        Comma => RETROK_COMMA,
        Minus => RETROK_MINUS,
        Period => RETROK_PERIOD,
        Slash => RETROK_SLASH,
        Digit0 => RETROK_0,
        Digit1 => RETROK_1,
        Digit2 => RETROK_2,
        Digit3 => RETROK_3,
        Digit4 => RETROK_4,
        Digit5 => RETROK_5,
        Digit6 => RETROK_6,
        Digit7 => RETROK_7,
        Digit8 => RETROK_8,
        Digit9 => RETROK_9,
        Semicolon => RETROK_SEMICOLON,
        Equal => RETROK_EQUALS,
        BracketLeft => RETROK_LEFTBRACKET,
        Backslash => RETROK_BACKSLASH,
        BracketRight => RETROK_RIGHTBRACKET,
        Backquote => RETROK_BACKQUOTE,
        KeyA => RETROK_a,
        KeyB => RETROK_b,
        KeyC => RETROK_c,
        KeyD => RETROK_d,
        KeyE => RETROK_e,
        KeyF => RETROK_f,
        KeyG => RETROK_g,
        KeyH => RETROK_h,
        KeyI => RETROK_i,
        KeyJ => RETROK_j,
        KeyK => RETROK_k,
        KeyL => RETROK_l,
        KeyM => RETROK_m,
        KeyN => RETROK_n,
        KeyO => RETROK_o,
        KeyP => RETROK_p,
        KeyQ => RETROK_q,
        KeyR => RETROK_r,
        KeyS => RETROK_s,
        KeyT => RETROK_t,
        KeyU => RETROK_u,
        KeyV => RETROK_v,
        KeyW => RETROK_w,
        KeyX => RETROK_x,
        KeyY => RETROK_y,
        KeyZ => RETROK_z,
        Delete => RETROK_DELETE,
        Insert => RETROK_INSERT,
        Home => RETROK_HOME,
        End => RETROK_END,
        PageUp => RETROK_PAGEUP,
        PageDown => RETROK_PAGEDOWN,
        ArrowUp => RETROK_UP,
        ArrowDown => RETROK_DOWN,
        ArrowLeft => RETROK_LEFT,
        ArrowRight => RETROK_RIGHT,
        F1 => RETROK_F1,
        F2 => RETROK_F2,
        F3 => RETROK_F3,
        F4 => RETROK_F4,
        F5 => RETROK_F5,
        F6 => RETROK_F6,
        F7 => RETROK_F7,
        F8 => RETROK_F8,
        F9 => RETROK_F9,
        F10 => RETROK_F10,
        F11 => RETROK_F11,
        F12 => RETROK_F12,
        ShiftLeft => RETROK_LSHIFT,
        ShiftRight => RETROK_RSHIFT,
        ControlLeft => RETROK_LCTRL,
        ControlRight => RETROK_RCTRL,
        AltLeft => RETROK_LALT,
        AltRight => RETROK_RALT,
        SuperLeft => RETROK_LSUPER,
        SuperRight => RETROK_RSUPER,
        CapsLock => RETROK_CAPSLOCK,
        _ => return None,
    })
}

fn retro_mods(mods: winit::keyboard::ModifiersState) -> u16 {
    use libretro::*;
    let mut out = RETROKMOD_NONE;
    if mods.shift_key() {
        out |= RETROKMOD_SHIFT;
    }
    if mods.control_key() {
        out |= RETROKMOD_CTRL;
    }
    if mods.alt_key() {
        out |= RETROKMOD_ALT;
    }
    if mods.super_key() {
        out |= RETROKMOD_META;
    }
    out as u16
}

// -------------------------------------------------------------------------
// Rendering
// -------------------------------------------------------------------------

const LOTTES_WGSL: &str = include_str!("../../../system/shaders/lottes.wgsl");

/// The one Bevy-ism in lottes.wgsl: a fullscreen vertex shader it imports.
const BEVY_IMPORT: &str =
    "#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput";

const FULLSCREEN_VS: &str = r#"
struct FullscreenVertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vertex(@builtin(vertex_index) index: u32) -> FullscreenVertexOutput {
    let uv = vec2<f32>(f32(index >> 1u), f32(index & 1u)) * 2.0;
    let clip = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    return FullscreenVertexOutput(clip, uv);
}
"#;

/// `PostProcessUniform` as lottes.wgsl declares it, padded to the 16-byte
/// stride a uniform buffer needs.
fn uniform_bytes(uv_scale: [f32; 2], uv_offset: [f32; 2], crt_enabled: bool) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, v) in [uv_scale[0], uv_scale[1], uv_offset[0], uv_offset[1]]
        .iter()
        .enumerate()
    {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
    }
    out[16..20].copy_from_slice(&u32::from(crt_enabled).to_ne_bytes());
    out
}

/// Where the picture lands in a window of `win` pixels, at display aspect
/// `aspect`: as large as fits, centred, aspect preserved.
fn fit_rect(win: (f32, f32), aspect: f32) -> (f32, f32, f32, f32) {
    let (win_w, win_h) = win;
    let aspect = if aspect > 0.0 { aspect } else { win_w / win_h };
    let (mut w, mut h) = (win_w, win_w / aspect);
    if h > win_h {
        h = win_h;
        w = win_h * aspect;
    }
    (
        ((win_w - w) * 0.5).floor(),
        ((win_h - h) * 0.5).floor(),
        w.floor().max(1.0),
        h.floor().max(1.0),
    )
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// Display aspect the frame is presented at.
    aspect: f32,
}

impl Gfx {
    fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(
            wgpu::InstanceDescriptor::new_without_display_handle_from_env()
                .with_display_handle(Box::new(window.clone())),
        );
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("c64"),
                required_limits: wgpu::Limits::downlevel_defaults(),
                ..Default::default()
            }))?;

        let caps = surface.get_capabilities(&adapter);
        // The shader outputs linear light and lets the target encode it.
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lottes"),
            source: wgpu::ShaderSource::Wgsl(
                LOTTES_WGSL.replace(BEVY_IMPORT, FULLSCREEN_VS).into(),
            ),
        });

        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lottes"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lottes"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lottes"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lottes settings"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&uniform, 0, &uniform_bytes([1.0, 1.0], [0.0, 0.0], true));

        let texture = create_frame_texture(&device, 1, 1);
        let bind_group = create_bind_group(&device, &bind_layout, &texture, &sampler, &uniform);

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_layout,
            sampler,
            uniform,
            texture,
            bind_group,
            aspect: 0.0,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.config.width = size.width.max(1);
        self.config.height = size.height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    fn upload(&mut self, width: u32, height: u32, pixels: &[u32]) {
        if self.texture.width() != width || self.texture.height() != height {
            self.texture = create_frame_texture(&self.device, width, height);
            self.bind_group = create_bind_group(
                &self.device,
                &self.bind_layout,
                &self.texture,
                &self.sampler,
                &self.uniform,
            );
        }
        self.queue.write_texture(
            self.texture.as_image_copy(),
            frame_bytes(&pixels[..(width * height) as usize]),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn render(&mut self) {
        use wgpu::CurrentSurfaceTexture as Acquired;
        let frame = match self.surface.get_current_texture() {
            Acquired::Success(frame) | Acquired::Suboptimal(frame) => frame,
            Acquired::Timeout | Acquired::Occluded => return,
            _ => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lottes"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            let win = (self.config.width as f32, self.config.height as f32);
            let (x, y, w, h) = fit_rect(win, self.aspect);
            pass.set_viewport(x, y, w, h, 0.0, 1.0);
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([encoder.finish()]);
        frame.present();
    }
}

fn create_frame_texture(device: &wgpu::Device, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("frame"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        // NON-sRGB on purpose: libretro cores deliver gamma-encoded pixels and
        // the shader does the decode itself.
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
    uniform: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let view = texture.create_view(&Default::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lottes"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: uniform.as_entire_binding(),
            },
        ],
    })
}

// -------------------------------------------------------------------------
// App
// -------------------------------------------------------------------------

pub struct App {
    core: RetroCoreDirect,
    gfx: Option<Gfx>,
    mods: u16,
    frame_time: Duration,
    next_frame: Instant,
}

impl App {
    pub fn new(core: RetroCoreDirect) -> Self {
        let fps = if core.fps() > 1.0 { core.fps() } else { 50.0 };
        Self {
            core,
            gfx: None,
            mods: 0,
            frame_time: Duration::from_secs_f64(1.0 / fps),
            next_frame: Instant::now(),
        }
    }

    /// Step the core one frame and hand the result to the GPU. Does nothing
    /// without a surface: on Android that is the app being in the background.
    fn step(&mut self) {
        if self.gfx.is_none() {
            return;
        }
        self.core.run();
        // Nothing plays the samples yet, but they have to be taken or the
        // core's buffer grows without bound.
        self.core.with_audio(|_| {});
        let Some(gfx) = self.gfx.as_mut() else {
            return;
        };
        gfx.aspect = self.core.aspect_ratio();
        let (width, height) = self.core.get_frame_size();
        if width == 0 || height == 0 {
            return;
        }
        self.core
            .with_frame(|w, h, pixels| gfx.upload(w as u32, h as u32, pixels));
        gfx.window.request_redraw();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("demarc C64")
            .with_inner_size(PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
        match event_loop
            .create_window(attrs)
            .map_err(anyhow::Error::from)
            .and_then(|window| Gfx::new(Arc::new(window)))
        {
            Ok(gfx) => self.gfx = Some(gfx),
            Err(e) => {
                eprintln!("Could not open a window: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.gfx = None;
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size);
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.mods = retro_mods(mods.state()),
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        state,
                        repeat: false,
                        ..
                    },
                ..
            } => {
                if let Some(key) = retro_key(code) {
                    let down = state == ElementState::Pressed;
                    self.core.press_key(key, down, self.mods);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.render();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if now >= self.next_frame {
            // Skip missed frames rather than trying to catch up on them.
            self.next_frame = (self.next_frame + self.frame_time).max(now);
            self.step();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

pub fn load_core() -> Result<RetroCoreDirect> {
    let path = find_core(CORE_NAME).ok_or_else(|| {
        anyhow!(
            "could not find {} — set DEMARC_CORE_DIR, or run demarc once to \
             download it into the core cache",
            dylib_name(CORE_NAME)
        )
    })?;
    let system = system_dir();
    info!("Core: {}", path.display());
    info!("System dir: {}", system.display());
    RetroCoreDirect::new(&path, &system, None, HashMap::new())
        .with_context(|| format!("could not start {}", path.display()))
}

// -------------------------------------------------------------------------
// Android entry point
// -------------------------------------------------------------------------

/// What `android-activity`'s NativeActivity glue calls on its own thread, in
/// place of `main`. Declared `extern "Rust"` there, so no `extern "C"` here.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use winit::event_loop::EventLoop;
    use winit::platform::android::EventLoopBuilderExtAndroid;

    crate::android::init_logging();

    // The app's private data directory is the only writable place, and is what
    // the core gets as its system and save directory.
    if let Some(dir) = app.internal_data_path() {
        crate::set_system_dir(dir);
    }

    let run = || -> Result<()> {
        let core = load_core()?;
        let event_loop = EventLoop::builder().with_android_app(app).build()?;
        event_loop.run_app(&mut App::new(core))?;
        Ok(())
    };
    if let Err(e) = run() {
        tracing::error!("{e:#}");
    }
}
