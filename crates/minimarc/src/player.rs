//! The window: the VICE libretro core through the lottes CRT shader, with the
//! picker drawn over it.

#![allow(dead_code)]

use retro_core::libretro;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tracing::info;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, Touch, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{Window, WindowId};

use retro_core::audio::{AudioSink, rate_adjust, smooth_fill};
use retro_core::backend::{InputEvent, frame_bytes};
use retro_core::retro_emu::RetroCoreDirect;
use retro_core::{dylib_name, find_core, system_dir};

use crate::load::{Demo, Loaded, Loader};
use crate::media::{self, Media};
use crate::ui::{Action, BUTTONS, Button, Ui};
use release_io::workfile::WorkFile;

const CORE_NAME: &str = "vice_x64sc";
const WINDOW_SIZE: (u32, u32) = (720, 576);

/// How far a finger has to travel, in physical pixels, before it is a swipe,
/// and how long it may take to get there.
const SWIPE_DIST: f64 = 80.0;
/// Phone text is read at arm's length on a small screen.
const UI_ZOOM: f32 = if cfg!(target_os = "android") {
    2.0
} else {
    1.0
};
/// Window height in logical pixels past which the UI stops growing with it.
const UI_MAX_HEIGHT: f32 = if cfg!(target_os = "android") {
    600.0
} else {
    f32::INFINITY
};
const SWIPE_TIME: Duration = Duration::from_millis(500);
/// A second headset tap within this skips to the next release.
const DOUBLE_TAP: Duration = Duration::from_millis(400);
/// Core frames a key is held at least, see `App::flush_keys`.
const MIN_KEY_FRAMES: u32 = 3;

// -------------------------------------------------------------------------
// Keyboard
// -------------------------------------------------------------------------

/// winit physical key → libretro keycode, for the keys a C64 has (or that VICE
/// maps onto one).
fn key_table() -> &'static [(KeyCode, libretro::retro_key)] {
    use KeyCode::*;
    use libretro::*;
    &[
        (Backspace, RETROK_BACKSPACE),
        (Tab, RETROK_TAB),
        (Enter, RETROK_RETURN),
        (NumpadEnter, RETROK_RETURN),
        (Escape, RETROK_ESCAPE),
        (Space, RETROK_SPACE),
        (Quote, RETROK_QUOTE),
        (Comma, RETROK_COMMA),
        (Minus, RETROK_MINUS),
        (Period, RETROK_PERIOD),
        (Slash, RETROK_SLASH),
        (Digit0, RETROK_0),
        (Digit1, RETROK_1),
        (Digit2, RETROK_2),
        (Digit3, RETROK_3),
        (Digit4, RETROK_4),
        (Digit5, RETROK_5),
        (Digit6, RETROK_6),
        (Digit7, RETROK_7),
        (Digit8, RETROK_8),
        (Digit9, RETROK_9),
        (Semicolon, RETROK_SEMICOLON),
        (Equal, RETROK_EQUALS),
        (BracketLeft, RETROK_LEFTBRACKET),
        (Backslash, RETROK_BACKSLASH),
        (BracketRight, RETROK_RIGHTBRACKET),
        (Backquote, RETROK_BACKQUOTE),
        (KeyA, RETROK_a),
        (KeyB, RETROK_b),
        (KeyC, RETROK_c),
        (KeyD, RETROK_d),
        (KeyE, RETROK_e),
        (KeyF, RETROK_f),
        (KeyG, RETROK_g),
        (KeyH, RETROK_h),
        (KeyI, RETROK_i),
        (KeyJ, RETROK_j),
        (KeyK, RETROK_k),
        (KeyL, RETROK_l),
        (KeyM, RETROK_m),
        (KeyN, RETROK_n),
        (KeyO, RETROK_o),
        (KeyP, RETROK_p),
        (KeyQ, RETROK_q),
        (KeyR, RETROK_r),
        (KeyS, RETROK_s),
        (KeyT, RETROK_t),
        (KeyU, RETROK_u),
        (KeyV, RETROK_v),
        (KeyW, RETROK_w),
        (KeyX, RETROK_x),
        (KeyY, RETROK_y),
        (KeyZ, RETROK_z),
        (Delete, RETROK_DELETE),
        (Insert, RETROK_INSERT),
        (Home, RETROK_HOME),
        (End, RETROK_END),
        (PageUp, RETROK_PAGEUP),
        (PageDown, RETROK_PAGEDOWN),
        (ArrowUp, RETROK_UP),
        (ArrowDown, RETROK_DOWN),
        (ArrowLeft, RETROK_LEFT),
        (ArrowRight, RETROK_RIGHT),
        (F1, RETROK_F1),
        (F2, RETROK_F2),
        (F3, RETROK_F3),
        (F4, RETROK_F4),
        (F5, RETROK_F5),
        (F6, RETROK_F6),
        (F7, RETROK_F7),
        (F8, RETROK_F8),
        (F9, RETROK_F9),
        (F10, RETROK_F10),
        (F11, RETROK_F11),
        (F12, RETROK_F12),
        (ShiftLeft, RETROK_LSHIFT),
        (ShiftRight, RETROK_RSHIFT),
        (ControlLeft, RETROK_LCTRL),
        (ControlRight, RETROK_RCTRL),
        (AltLeft, RETROK_LALT),
        (AltRight, RETROK_RALT),
        (SuperLeft, RETROK_LSUPER),
        (SuperRight, RETROK_RSUPER),
        (CapsLock, RETROK_CAPSLOCK),
    ]
}

fn retro_key(key: KeyCode) -> Option<libretro::retro_key> {
    let (_, code) = key_table().iter().find(|(k, _)| *k == key)?;
    Some(*code)
}

/// A key as `overrides.toml` names it: a `KeyCode` name, or a lone letter or digit.
pub(crate) fn key_by_name(key: &str) -> Option<libretro::retro_key> {
    let name = match key {
        c if c.len() == 1 && c.as_bytes()[0].is_ascii_digit() => format!("Digit{c}"),
        c if c.len() == 1 && c.as_bytes()[0].is_ascii_alphabetic() => {
            format!("Key{}", c.to_ascii_uppercase())
        }
        name => name.to_string(),
    };
    let (_, code) = key_table().iter().find(|(k, _)| format!("{k:?}") == name)?;
    Some(*code)
}

/// The character a key types, if it types exactly one.
fn typed_char(key: &Key) -> Option<char> {
    let Key::Character(s) = key else { return None };
    let mut chars = s.chars();
    chars.next().filter(|_| chars.next().is_none())
}

/// The keys a C64 needs that a compact keyboard does not have: Alt picks a
/// digit, Shift+Alt the function key above it.
fn alt_key(key: KeyCode, shift: bool) -> Option<libretro::retro_key> {
    use KeyCode::*;
    use libretro::*;
    let i = match key {
        KeyQ => 0,
        KeyW => 1,
        KeyE => 2,
        KeyR => 3,
        KeyS => 4,
        KeyD => 5,
        KeyF => 6,
        KeyZ => 7,
        KeyX => 8,
        KeyC => 9,
        _ => return None,
    };
    let digits = [
        RETROK_0, RETROK_1, RETROK_2, RETROK_3, RETROK_4, RETROK_5, RETROK_6, RETROK_7, RETROK_8,
        RETROK_9,
    ];
    let fkeys = [
        RETROK_ESCAPE,
        RETROK_F1,
        RETROK_F2,
        RETROK_F3,
        RETROK_F4,
        RETROK_F5,
        RETROK_F6,
        RETROK_F7,
    ];
    if shift {
        fkeys.get(i).copied()
    } else {
        Some(digits[i])
    }
}

/// The punctuation the other Alt combos reach, as the C64 key at that place
/// plus whether shift is part of the symbol. VICE's default keymap is
/// positional and has no entry for RETROK_DOLLAR and friends, so a shifted
/// symbol has to arrive as a genuine shifted keypress.
fn alt_symbol(key: KeyCode) -> Option<(libretro::retro_key, bool)> {
    use KeyCode::*;
    use libretro::*;
    Some(match key {
        KeyP => (RETROK_4, true),          // $
        KeyJ => (RETROK_3, true),          // #
        KeyL => (RETROK_2, true),          // "
        KeyN => (RETROK_COMMA, false),     // ,
        KeyM => (RETROK_PERIOD, false),    // .
        KeyO => (RETROK_MINUS, false),     // +
        KeyI => (RETROK_EQUALS, false),    // -
        KeyH => (RETROK_SEMICOLON, false), // :
        KeyK => (RETROK_QUOTE, false),     // ;
        KeyU => (RETROK_BACKQUOTE, false), // the C64's own left-arrow key
        _ => return None,
    })
}

// -------------------------------------------------------------------------
// Rendering
// -------------------------------------------------------------------------

const CRT_WGSL: &str = include_str!("crt.wgsl");

const FRAGMENT: &str = "fragment_normal";

/// Where the picture lands in a window of `win` pixels, at display aspect
/// `aspect`: as large as fits, centred, aspect preserved.
/// Gap, in points, between the key bar and the soft keyboard.
const KEYBOARD_MARGIN: f32 = 16.0;

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
    instance: wgpu::Instance,
    /// None while the app is in the background on Android.
    surface: Option<wgpu::Surface<'static>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    /// Display aspect the frame is presented at.
    aspect: f32,
    /// Whether `texture` holds a frame of the core that is running.
    has_frame: bool,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

impl Gfx {
    fn new(window: Arc<Window>, ctx: &egui::Context) -> Result<Self> {
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
            apply_limit_buckets: false,
        }))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("c64"),
                required_limits:
                    wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
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
            color_space: Default::default(),
        };
        surface.configure(&device, &config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("crt"),
            source: wgpu::ShaderSource::Wgsl(CRT_WGSL.into()),
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
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lottes"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(FRAGMENT),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(FRAGMENT),
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

        let texture = create_frame_texture(&device, 1, 1);
        let bind_group = create_bind_group(&device, &bind_layout, &texture, &sampler);

        let egui_state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, Default::default());

        Ok(Self {
            window,
            instance,
            surface: Some(surface),
            device,
            queue,
            config,
            pipeline,
            bind_layout,
            sampler,
            texture,
            bind_group,
            aspect: 0.0,
            has_frame: false,
            egui_state,
            egui_renderer,
        })
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.config.width = size.width.max(1);
        self.config.height = size.height.max(1);
        if let Some(surface) = &self.surface {
            surface.configure(&self.device, &self.config);
        }
    }

    /// A new surface for the resumed activity. The device and the egui
    /// renderer stay, since the egui context won't send its textures again.
    fn resume(&mut self) -> Result<()> {
        self.surface = Some(self.instance.create_surface(self.window.clone())?);
        self.resize(self.window.inner_size());
        Ok(())
    }

    fn upload(&mut self, width: u32, height: u32, pixels: &[u32]) {
        self.has_frame = true;
        if self.texture.width() != width || self.texture.height() != height {
            self.texture = create_frame_texture(&self.device, width, height);
            self.bind_group = create_bind_group(
                &self.device,
                &self.bind_layout,
                &self.texture,
                &self.sampler,
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

    /// Run one egui frame over `ui`, and return what it wants drawn along with
    /// the entry the user picked.
    fn run_ui(&mut self, ui: &mut Ui) -> (UiFrame, Option<Action>) {
        let scale = self.window.scale_factor() as f32;
        let height = (self.config.height as f32 / scale).min(UI_MAX_HEIGHT);
        retro_ui::set_scale(&ui.ctx, height, scale, UI_ZOOM);
        let mut input = self.egui_state.take_egui_input(&self.window);
        // Android's focus event comes before there is an egui state to take it.
        input.focused = self.window.has_focus();
        input.safe_area_insets = Some(egui::SafeAreaInsets(
            self.safe_area() / ui.ctx.pixels_per_point(),
        ));
        let ppp = ui.ctx.pixels_per_point();
        let frame_rect = self.frame_rect(ui.key_bar_height() * ppp);
        let portrait = self.config.height > self.config.width;
        ui.bar_top = (portrait && self.has_frame).then(|| (frame_rect.1 + frame_rect.3) / ppp);
        let ctx = ui.ctx.clone();
        let mut picked = None;
        let mut output = ctx.run_ui(input, |egui_ui| picked = ui.draw(egui_ui.ctx()));
        // The soft keyboard covers the picker's buttons.
        if cfg!(target_os = "android") {
            let ime = &mut output.platform_output.ime;
            if !ui.soft_keyboard {
                *ime = None;
            } else if ime.is_none() {
                // Asked for from the key bar, with no text field to focus.
                *ime = Some(egui::output::IMEOutput {
                    purpose: Default::default(),
                    rect: egui::Rect::NOTHING,
                    cursor_rect: egui::Rect::NOTHING,
                    should_interrupt_composition: false,
                });
            }
        }
        self.egui_state
            .handle_platform_output(&self.window, output.platform_output);
        let repaint = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .is_some_and(|v| v.repaint_delay.is_zero());
        let frame = UiFrame {
            repaint,
            frame_rect,
            jobs: ctx.tessellate(output.shapes, output.pixels_per_point),
            textures: output.textures_delta,
            pixels_per_point: output.pixels_per_point,
        };
        (frame, picked)
    }

    /// Where the frame goes, in pixels: in portrait, high enough that the key
    /// bar, `bar` pixels tall, fits under it above the soft keyboard.
    fn frame_rect(&self, bar: f32) -> (f32, f32, f32, f32) {
        let win = (self.config.width as f32, self.config.height as f32);
        let (x, mut y, w, h) = fit_rect(win, self.aspect);
        if win.1 > win.0 {
            let margin = KEYBOARD_MARGIN * self.window.scale_factor() as f32;
            let bottom = win.1 - self.safe_area().bottom - margin - bar;
            y = y.min((bottom - h).floor()).max(0.0);
        }
        (x, y, w, h)
    }

    /// Physical pixels on each side covered by the camera cutout, rounded
    /// corners or (in portrait) the soft keyboard: `MiniActivity` pads the
    /// content view by them.
    fn safe_area(&self) -> egui::epaint::MarginF32 {
        #[cfg(target_os = "android")]
        {
            use winit::platform::android::WindowExtAndroid;
            let r = self.window.content_rect();
            let px = |v: i32| v.max(0) as f32;
            return egui::epaint::MarginF32 {
                left: px(r.left),
                top: px(r.top),
                right: px(self.config.width as i32 - r.right),
                bottom: px(self.config.height as i32 - r.bottom),
            };
        }
        #[allow(unreachable_code)]
        Default::default()
    }

    fn render(&mut self, mut ui: UiFrame) {
        // Before anything that can bail out: egui panics over a delta that is
        // dropped unapplied, and a skipped frame's textures are still current.
        for (id, deltas) in &ui.textures.set {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }
        let free = std::mem::take(&mut ui.textures.free);
        ui.textures.clear();
        self.draw(&ui);
        for id in &free {
            self.egui_renderer.free_texture(id);
        }
    }

    fn draw(&mut self, ui: &UiFrame) {
        use wgpu::CurrentSurfaceTexture as Acquired;
        let Some(surface) = &self.surface else {
            return;
        };
        let frame = match surface.get_current_texture() {
            Acquired::Success(frame) | Acquired::Suboptimal(frame) => frame,
            Acquired::Timeout | Acquired::Occluded => return,
            _ => {
                surface.configure(&self.device, &self.config);
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.config.width, self.config.height],
            pixels_per_point: ui.pixels_per_point,
        };
        let extra = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &ui.jobs,
            &screen,
        );
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
            if self.has_frame {
                let (x, y, w, h) = ui.frame_rect;
                pass.set_viewport(x, y, w, h, 0.0, 1.0);
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &ui.jobs, &screen);
        }
        self.queue
            .submit(extra.into_iter().chain([encoder.finish()]));
        self.queue.present(frame);
    }
}

/// What one egui frame left to draw.
struct UiFrame {
    /// egui wants the next frame at once, e.g. a list still scrolling.
    repaint: bool,
    /// Where the emulator frame goes, in pixels.
    frame_rect: (f32, f32, f32, f32),
    jobs: Vec<egui::ClippedPrimitive>,
    textures: egui::TexturesDelta,
    pixels_per_point: f32,
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
        // libretro pixels are gamma-encoded: sampling decodes them to linear.
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

fn create_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
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
        ],
    })
}

// -------------------------------------------------------------------------
// Audio
// -------------------------------------------------------------------------

/// Sink fill, in samples, the drift control steers between. Under the lower
/// one an extra frame is run rather than let the ring run dry.
const AUDIO_BUF: (usize, usize) = (3000, 18000);

// -------------------------------------------------------------------------
// Touch
// -------------------------------------------------------------------------

/// One finger on the screen.
struct Swipe {
    id: u64,
    start: (f64, f64),
    began: Instant,
}

/// The key a key bar button holds down, if it is a key.
fn bar_key(button: Button) -> Option<libretro::retro_key> {
    use libretro::*;
    Some(match button {
        Button::F1 => RETROK_F1,
        Button::F3 => RETROK_F3,
        Button::F5 => RETROK_F5,
        Button::F7 => RETROK_F7,
        Button::Space => RETROK_SPACE,
        Button::Return => RETROK_RETURN,
        _ => return None,
    })
}

const WARP_TIME: Duration = Duration::from_secs(10);

/// The cursor key a movement of `(dx, dy)` means, once it is long enough.
fn swipe_key(dx: f64, dy: f64) -> Option<libretro::retro_key> {
    use libretro::*;
    if dx.hypot(dy) < SWIPE_DIST {
        return None;
    }
    Some(if dx.abs() > dy.abs() {
        if dx > 0.0 { RETROK_RIGHT } else { RETROK_LEFT }
    } else if dy > 0.0 {
        RETROK_DOWN
    } else {
        RETROK_UP
    })
}

// -------------------------------------------------------------------------
// App
// -------------------------------------------------------------------------

/// How long a scheduled startup key is held, in frames.
const KEY_HOLD: u32 = 2;

pub struct App {
    core: Option<RetroCoreDirect>,
    /// The release `core` is running, kept for the temp dir it may sit in.
    work: Option<WorkFile>,
    /// Keys to press once `core` has run that many frames.
    startup_keys: Vec<(u32, libretro::retro_key, bool)>,
    core_frames: u32,
    ui: Ui,
    loader: Loader,
    gfx: Option<Gfx>,
    audio: AudioSink,
    /// Smoothed fill of `audio`, see `smooth_fill`.
    audio_fill: f64,
    alt: bool,
    shift: bool,
    ctrl: bool,
    /// What each held physical key sent, so its release cancels that key even
    /// if the modifiers changed in between, and the shift it forced on the C64.
    pressed: HashMap<KeyCode, (libretro::retro_key, Option<bool>)>,
    /// Key changes not yet given to the core, see `flush_keys`.
    key_queue: VecDeque<(libretro::retro_key, bool, u16)>,
    /// The core frame each key the core has down went down on.
    keys_down: HashMap<libretro::retro_key, u32>,
    last_key_frame: Option<u32>,
    /// Every finger currently down, so a second one can veto the swipe.
    touches: Vec<u64>,
    swipe: Option<Swipe>,
    /// The finger on a key bar button or after a swipe, and the key it holds down.
    bar_key: Option<(u64, libretro::retro_key)>,
    /// The finger on a key bar button that acts when lifted, and its index.
    bar_press: Option<(u64, usize)>,
    /// The finger that went down on the open picker, and where.
    nav_swipe: Option<(u64, (f64, f64))>,
    frame_time: Duration,
    next_frame: Instant,
    warp_until: Option<Instant>,
    disk: u32,
    /// A headset tap waiting to see if a second one follows.
    tap_at: Option<Instant>,
}

impl App {
    /// An app browsing the database at `db`.
    pub fn new(db: PathBuf) -> Self {
        let mut ui = Ui::new();
        ui.status("Loading database...");
        let loader = Loader::new();
        loader.load_db(db);
        Self {
            core: None,
            work: None,
            startup_keys: Vec::new(),
            core_frames: 0,
            ui,
            loader,
            gfx: None,
            audio: AudioSink::default(),
            audio_fill: 0.0,
            alt: false,
            shift: false,
            ctrl: false,
            pressed: HashMap::new(),
            key_queue: VecDeque::new(),
            keys_down: HashMap::new(),
            last_key_frame: None,
            touches: Vec::new(),
            swipe: None,
            bar_key: None,
            bar_press: None,
            nav_swipe: None,
            frame_time: Duration::from_secs_f64(1.0 / 50.0),
            next_frame: Instant::now(),
            warp_until: None,
            disk: 0,
            tap_at: None,
        }
    }

    fn press(&mut self, key: libretro::retro_key, down: bool, mods: u16) {
        self.key_queue.push_back((key, down, mods));
        self.flush_keys();
    }

    /// Act on what the loader threads have finished.
    fn poll_loader(&mut self) {
        while let Some(loaded) = self.loader.poll() {
            let result = match loaded {
                Loaded::Db(db) => db.and_then(|(name, files)| self.ui.set_db(name, files)),
                Loaded::Demo(_, demo) => demo.and_then(|demo| self.start(demo)),
            };
            if let Err(e) = result {
                tracing::error!("{e:#}");
                self.ui.error(&format!("{e:#}"));
                self.ui.open();
            }
        }
    }

    /// Replace the running core with one booting `demo`.
    fn start(&mut self, demo: Demo) -> Result<()> {
        let Demo { work, events, info } = demo;
        self.cancel_touch();
        self.pressed.clear();
        self.key_queue.clear();
        self.keys_down.clear();
        self.last_key_frame = None;
        // One core at a time: Android loads it in place, so the old instance
        // has to be gone before the new one starts.
        self.core = None;
        self.work = None;
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.has_frame = false;
        }
        let core = load_core(Some(&work.path), work.get_all_meta())?;
        let fps = if core.fps() > 1.0 { core.fps() } else { 50.0 };
        self.frame_time = Duration::from_secs_f64(1.0 / fps);
        self.startup_keys.clear();
        for (frame, event) in events {
            if let InputEvent::Key(key) = event {
                self.startup_keys.push((frame, key, true));
                self.startup_keys.push((frame + KEY_HOLD, key, false));
            }
        }
        self.core_frames = 0;
        self.audio_fill = 0.0;
        self.ui.paused = false;
        self.warp_until = None;
        self.disk = 0;
        self.core = Some(core);
        self.work = Some(work);
        self.ui.info(&info);
        Ok(())
    }

    fn mods(&self) -> u16 {
        use libretro::*;
        let mut out = RETROKMOD_NONE;
        // Alt only selects a key here, and a shift held with it picks a
        // function key, so neither reaches the core as a modifier.
        if self.shift && !self.alt {
            out |= RETROKMOD_SHIFT;
        }
        if self.ctrl {
            out |= RETROKMOD_CTRL;
        }
        out as u16
    }

    fn mods_with(&self, shift: Option<bool>) -> u16 {
        let bit = libretro::RETROKMOD_SHIFT as u16;
        match shift {
            Some(true) => self.mods() | bit,
            Some(false) => self.mods() & !bit,
            None => self.mods(),
        }
    }

    /// The key to send for `code`, typing `text`, and the shift it needs.
    fn key_for(
        &self,
        code: KeyCode,
        text: Option<char>,
    ) -> Option<(libretro::retro_key, Option<bool>)> {
        if self.alt {
            return alt_key(code, self.shift)
                .map(|key| (key, None))
                .or_else(|| alt_symbol(code).map(|(key, shifted)| (key, shifted.then_some(true))));
        }
        // A C64 has one Ctrl and no right shift.
        if code == KeyCode::ShiftRight {
            return Some((libretro::RETROK_LCTRL, None));
        }
        if !self.ctrl
            && let Some((key, shifted)) = text.and_then(release_io::c64::char_key)
        {
            return Some((key, Some(shifted)));
        }
        retro_key(code).map(|key| (key, None))
    }

    /// Modifiers are tracked here rather than taken from `ModifiersChanged`,
    /// which never arrives.
    fn key(&mut self, code: KeyCode, text: Option<char>, down: bool) {
        // The touchpad is over the keyboard, so typing drags fingers across it
        // and the lift of such a finger may never arrive. Drop the gesture and
        // everything it was waiting for rather than leave a cursor key down.
        self.cancel_touch();
        match code {
            KeyCode::AltLeft | KeyCode::AltRight => {
                self.alt = down;
                // Alt is a prefix, not a key, so a shift held across it has to
                // be lifted and put back.
                if down {
                    self.send(KeyCode::ShiftLeft, None, false);
                } else if self.shift {
                    self.send(KeyCode::ShiftLeft, None, true);
                }
                return;
            }
            KeyCode::ShiftLeft => self.shift = down,
            KeyCode::ShiftRight => self.ctrl = down,
            KeyCode::Backspace if down && self.alt && self.shift => {
                if let Some(core) = self.core.as_mut() {
                    core.reset();
                }
                return;
            }
            _ => {}
        }
        self.send(code, text, down);
    }

    fn send(&mut self, code: KeyCode, text: Option<char>, down: bool) {
        let held = self.shift && !self.alt;
        if !down {
            if let Some((key, shift)) = self.pressed.remove(&code) {
                self.press(key, false, self.mods_with(shift));
                if shift.is_some_and(|want| want != held) {
                    self.press(libretro::RETROK_LSHIFT, held, self.mods());
                }
            }
        } else if !self.pressed.contains_key(&code)
            && let Some((key, shift)) = self.key_for(code, text)
        {
            let shift = shift.filter(|&want| want != held);
            self.pressed.insert(code, (key, shift));
            let mods = self.mods_with(shift);
            if let Some(want) = shift {
                self.press(libretro::RETROK_LSHIFT, want, mods);
            }
            self.press(key, true, mods);
        }
    }

    /// Hand queued key changes to the core, one per frame: VICE only sees a
    /// shift that was down before the key, and the soft keyboard sends shift,
    /// key and both releases at once. A release also waits until its key has
    /// been down for `MIN_KEY_FRAMES`, or the C64 never scans it.
    fn flush_keys(&mut self) {
        let Some(core) = self.core.as_mut() else {
            return;
        };
        let now = self.core_frames;
        while let Some(&(key, down, mods)) = self.key_queue.front() {
            if self.last_key_frame == Some(now) {
                break;
            }
            if down {
                self.keys_down.insert(key, now);
            } else if let Some(&at) = self.keys_down.get(&key) {
                if at + MIN_KEY_FRAMES > now {
                    break;
                }
                self.keys_down.remove(&key);
            }
            self.key_queue.pop_front();
            tracing::info!("KEYDBG f{now} press {key} {down} {mods}");
            core.press_key(key, down, mods);
            self.last_key_frame = Some(now);
        }
    }

    /// A swipe is a cursor key, a long press opens or closes the key bar;
    /// keys are held for as long as the finger is on them. A second finger cancels a swipe and blocks
    /// another until the screen is clear: the touchpad sits on the keyboard, so
    /// typing drags fingers across it.
    fn touch(&mut self, t: Touch) {
        if self.bar_key.is_some_and(|(id, _)| id == t.id) {
            if matches!(t.phase, TouchPhase::Ended | TouchPhase::Cancelled) {
                self.touches.retain(|&id| id != t.id);
                self.release_bar_key();
            }
            return;
        }
        if let Some((id, i)) = self.bar_press
            && id == t.id
        {
            let over = self.ui.key_at(t.location.x, t.location.y) == Some(i);
            match t.phase {
                TouchPhase::Started | TouchPhase::Moved => {
                    self.ui.key_held = over.then_some(i);
                }
                TouchPhase::Ended | TouchPhase::Cancelled => {
                    self.bar_press = None;
                    self.ui.key_held = None;
                    if over && t.phase == TouchPhase::Ended {
                        self.bar_button(BUTTONS[i]);
                    }
                }
            }
            return;
        }
        match t.phase {
            TouchPhase::Started => {
                if let Some(i) = self.ui.key_at(t.location.x, t.location.y) {
                    self.release_bar_key();
                    if let Some(key) = bar_key(BUTTONS[i]) {
                        self.press(key, true, self.mods());
                        self.bar_key = Some((t.id, key));
                        self.ui.key_held = Some(i);
                    } else {
                        self.bar_press = Some((t.id, i));
                        self.ui.key_held = Some(i);
                    }
                    return;
                }
                self.swipe = None;
                if !self.touches.contains(&t.id) {
                    self.touches.push(t.id);
                }
                if self.touches.len() == 1 {
                    self.swipe = Some(Swipe {
                        id: t.id,
                        start: (t.location.x, t.location.y),
                        began: Instant::now(),
                    });
                }
            }
            TouchPhase::Moved => {
                let Some(swipe) = self.swipe.as_ref() else {
                    return;
                };
                if swipe.id != t.id || swipe.began.elapsed() > SWIPE_TIME {
                    return;
                }
                let (dx, dy) = (t.location.x - swipe.start.0, t.location.y - swipe.start.1);
                if let Some(key) = swipe_key(dx, dy) {
                    self.swipe = None;
                    self.release_bar_key();
                    self.press(key, true, self.mods());
                    self.bar_key = Some((t.id, key));
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.touches.retain(|&id| id != t.id);
                if let Some(swipe) = self.swipe.take_if(|s| s.id == t.id)
                    && t.phase == TouchPhase::Ended
                {
                    if swipe.began.elapsed() < SWIPE_TIME {
                        self.toggle_keys();
                    } else {
                        self.toggle_keys();
                    }
                }
            }
        }
    }

    fn toggle_keys(&mut self) {
        self.ui.keys_open = !self.ui.keys_open;
        self.ui.soft_keyboard = false;
    }

    /// A finger held still past `SWIPE_TIME` is a long press.
    fn check_long_press(&mut self) {
        if self
            .swipe
            .take_if(|s| s.began.elapsed() >= SWIPE_TIME)
            .is_some()
        {
            self.toggle_keys();
        }
    }

    fn bar_button(&mut self, button: Button) {
        match button {
            Button::Keyboard => {
                self.ui.soft_keyboard = !self.ui.soft_keyboard;
            }
            Button::Disk => {
                if let Some(core) = self.core.as_mut() {
                    let disks = core.get_number_of_disks();
                    if disks > 1 {
                        self.disk = (self.disk + 1) % disks;
                        core.set_disk(self.disk);
                    }
                }
            }
            Button::Reset => {
                if let Some(core) = self.core.as_mut() {
                    core.reset();
                }
            }
            Button::Pause => self.ui.paused = !self.ui.paused,
            Button::Warp => self.warp_until = Some(Instant::now() + WARP_TIME),
            Button::Next => self.skip(1),
            Button::Prev => self.skip(-1),
            Button::Back => self.ui.open(),
            _ => {}
        }
    }

    fn media(&mut self, m: Media) {
        match m {
            Media::Tap if self.tap_at.take().is_some() => self.skip(1),
            Media::Tap => self.tap_at = Some(Instant::now()),
            Media::Toggle => self.ui.paused = !self.ui.paused,
            Media::Next => self.skip(1),
            Media::Prev => self.skip(-1),
        }
    }

    /// Load the release `d` steps along the list the running one was picked from.
    fn skip(&mut self, d: isize) {
        if let Some(file) = self.ui.step(d) {
            self.ui.status("Loading...");
            self.loader.load_demo(file);
        }
    }

    /// A sideways swipe over the picker is Left or Right.
    fn nav_touch(&mut self, t: &Touch) {
        match t.phase {
            TouchPhase::Started => self.nav_swipe = Some((t.id, (t.location.x, t.location.y))),
            TouchPhase::Moved => {}
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let Some((id, start)) = self.nav_swipe.take() else {
                    return;
                };
                if id != t.id || t.phase == TouchPhase::Cancelled {
                    return;
                }
                match swipe_key(t.location.x - start.0, t.location.y - start.1) {
                    Some(libretro::RETROK_LEFT) => self.ui.back(),
                    Some(libretro::RETROK_RIGHT) => self.ui.forward(),
                    _ => {}
                }
            }
        }
    }

    fn cancel_touch(&mut self) {
        self.touches.clear();
        self.swipe = None;
        self.bar_press = None;
        self.release_bar_key();
    }

    /// Lifted a couple of frames on, so a tap shorter than a frame still lands.
    fn release_bar_key(&mut self) {
        self.ui.key_held = None;
        if let Some((_, key)) = self.bar_key.take() {
            self.startup_keys
                .push((self.core_frames + KEY_HOLD, key, false));
        }
    }

    /// Step the core one frame and hand the result to the GPU. Without a
    /// surface (on Android, in the background) the core only plays its sound.
    fn step(&mut self) {
        let Some(gfx) = self.gfx.as_ref() else {
            return;
        };
        let visible = gfx.surface.is_some();
        if visible {
            gfx.window.request_redraw();
            self.check_long_press();
        }
        self.poll_loader();
        for m in media::take() {
            self.media(m);
        }
        if self.tap_at.is_some_and(|t| t.elapsed() >= DOUBLE_TAP) {
            self.tap_at = None;
            self.ui.paused = !self.ui.paused;
        }
        if self.core.is_none() || self.ui.paused {
            return;
        }
        let now = Instant::now();
        if self.warp_until.is_some_and(|t| now < t) {
            let end = now + self.frame_time.mul_f32(0.8);
            while Instant::now() < end {
                self.run_frame();
            }
        } else {
            self.warp_until = None;
            // Timer jitter and a late frame both drain the ring.
            if self.run_frame() && self.audio.occupied_len() < AUDIO_BUF.0 {
                self.run_frame();
            }
        }
        self.audio_fill = smooth_fill(self.audio_fill, self.audio.occupied_len());
        let adjust = rate_adjust(self.audio_fill, AUDIO_BUF.0, AUDIO_BUF.1);
        self.audio.set_adjust(adjust);

        let (Some(core), Some(gfx)) = (self.core.as_mut(), self.gfx.as_mut()) else {
            return;
        };
        if !visible {
            return;
        }
        gfx.aspect = core.aspect_ratio();
        let (width, height) = core.get_frame_size();
        if width == 0 || height == 0 {
            return;
        }
        core.with_frame(|w, h, pixels| gfx.upload(w as u32, h as u32, pixels));
    }

    /// Run the core one frame and queue its sound. Whether there was any.
    fn run_frame(&mut self) -> bool {
        let Some(core) = self.core.as_mut() else {
            return false;
        };
        core.run();
        self.core_frames += 1;
        let frames = self.core_frames;
        self.startup_keys.retain(|&(frame, key, down)| {
            if frame <= frames {
                core.press_key(key, down, 0);
            }
            frame > frames
        });
        self.flush_keys();
        let Some(core) = self.core.as_mut() else {
            return false;
        };
        let rate = core.sample_rate() as f32;
        let audio = &mut self.audio;
        let warp = self.warp_until.is_some();
        let mut heard = false;
        core.with_audio(|samples| {
            heard = !samples.is_empty();
            if !warp {
                audio.push_audio(rate, samples);
            }
        });
        heard
    }

    /// Whether `event` was for the picker rather than the emulator.
    fn ui_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    physical_key,
                    logical_key,
                    state,
                    repeat,
                    ..
                },
            ..
        } = event
        {
            let down = *state == ElementState::Pressed;
            if let Some(m) = media::from_key(logical_key) {
                if down && !repeat {
                    self.media(m);
                }
                return true;
            }
            // Android's Back has a logical key only.
            let toggle = *logical_key == Key::Named(NamedKey::BrowserBack)
                || *physical_key == PhysicalKey::Code(KeyCode::Escape);
            match physical_key {
                _ if toggle => {
                    if down && !repeat {
                        if !self.ui.is_open() {
                            self.cancel_touch();
                            self.ui.open();
                        } else if self.core.is_some() {
                            self.ui.close();
                        }
                    }
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowLeft) if self.ui.is_open() => {
                    if down {
                        self.ui.back();
                    }
                    return true;
                }
                PhysicalKey::Code(KeyCode::ArrowRight) if self.ui.is_open() => {
                    if down {
                        self.ui.forward();
                    }
                    return true;
                }
                _ => {}
            }
        }
        if !self.ui.is_open() {
            return false;
        }
        if let WindowEvent::Touch(t) = event {
            self.nav_touch(t);
        }
        let input = matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::ModifiersChanged(_)
                | WindowEvent::Ime(_)
        );
        if let Some(gfx) = self.gfx.as_mut() {
            let _ = gfx.egui_state.on_window_event(&gfx.window, event);
        }
        input
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(gfx) = self.gfx.as_mut() {
            match gfx.resume() {
                Ok(()) if self.audio.failed() => {
                    self.audio.deactivate();
                    self.audio.activate();
                }
                Ok(()) => {}
                Err(e) => {
                    tracing::error!("Could not recreate the surface: {e:#}");
                    event_loop.exit();
                }
            }
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("demarc C64")
            .with_inner_size(PhysicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
        match event_loop
            .create_window(attrs)
            .map_err(anyhow::Error::from)
            .and_then(|window| Gfx::new(Arc::new(window), &self.ui.ctx))
        {
            Ok(gfx) => {
                self.gfx = Some(gfx);
                self.audio.activate();
            }
            Err(e) => {
                eprintln!("Could not open a window: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        self.cancel_touch();
        if let Some(gfx) = self.gfx.as_mut() {
            gfx.surface = None;
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let WindowEvent::KeyboardInput { event: e, .. } = &event {
            tracing::info!(
                "KEYDBG f{} event {:?} {:?} {:?}",
                self.core_frames,
                e.physical_key,
                e.logical_key,
                e.state
            );
        }
        if self.ui_event(&event) {
            if let Some(gfx) = self.gfx.as_ref() {
                gfx.window.request_redraw();
            }
            return;
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.resize(size);
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        physical_key: PhysicalKey::Code(code),
                        logical_key,
                        state,
                        repeat: false,
                        ..
                    },
                ..
            } => self.key(
                code,
                typed_char(&logical_key),
                state == ElementState::Pressed,
            ),
            WindowEvent::Touch(touch) => self.touch(touch),
            WindowEvent::Focused(false) => self.cancel_touch(),
            WindowEvent::RedrawRequested => {
                if let Some(gfx) = self.gfx.as_mut() {
                    let (frame, picked) = gfx.run_ui(&mut self.ui);
                    if frame.repaint {
                        gfx.window.request_redraw();
                    }
                    gfx.render(frame);
                    match picked {
                        Some(Action::Play(file)) => {
                            self.ui.status("Loading...");
                            self.loader.load_demo(file);
                        }
                        None => {}
                    }
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

fn load_core(
    game: Option<&std::path::Path>,
    settings: HashMap<String, String>,
) -> Result<RetroCoreDirect> {
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
    RetroCoreDirect::new(&path, &system, game, settings)
        .with_context(|| format!("could not start {}", path.display()))
}

// -------------------------------------------------------------------------
// Android entry point
// -------------------------------------------------------------------------

/// Files packaged in the APK (`android/app/src/main/assets/`), and where each
/// one is unpacked under the app's data directory. A core cannot read out of
/// the APK, and VICE looks for cartridge images in `<system dir>/vice/<machine>`.
#[cfg(target_os = "android")]
const ASSETS: [(&str, &str); 5] = [
    ("rr38ppal-auto.crt", "vice/C64/rr38ppal-auto.crt"),
    ("rr38ppal.crt", "vice/C64/rr38ppal.crt"),
    ("tar_v2_pal.crt", "vice/C64/tar_v2_pal.crt"),
    (DB_ASSET, DB_ASSET),
    (
        database::overrides::FILE_NAME,
        database::overrides::FILE_NAME,
    ),
];

#[cfg(target_os = "android")]
const DB_ASSET: &str = "csdb.txt";

/// Unconditionally, so a rebuilt APK replaces whatever the last one left behind.
#[cfg(target_os = "android")]
fn unpack_assets(
    app: &winit::platform::android::activity::AndroidApp,
    dir: &std::path::Path,
) -> Result<()> {
    use std::io::Read;

    let assets = app.asset_manager();
    for (name, dest) in ASSETS {
        let mut asset = assets
            .open(&std::ffi::CString::new(name)?)
            .ok_or_else(|| anyhow!("no asset {name} in the APK"))?;
        let mut data = Vec::new();
        asset.read_to_end(&mut data)?;
        let path = dir.join(dest);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &data)?;
        info!("Unpacked {} ({} bytes)", path.display(), data.len());
    }
    Ok(())
}

/// What `android-activity`'s NativeActivity glue calls on its own thread, in
/// place of `main`. Declared `extern "Rust"` there, so no `extern "C"` here.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use winit::event_loop::EventLoop;
    use winit::platform::android::EventLoopBuilderExtAndroid;

    retro_core::android::init_logging();
    // A dimmed screen throttles the CPU, and the demo with it.
    {
        use winit::platform::android::activity::WindowManagerFlags;
        app.set_window_flags(
            WindowManagerFlags::KEEP_SCREEN_ON,
            WindowManagerFlags::empty(),
        );
    }

    let run = || -> Result<()> {
        // The app's private data directory is the only writable place: the
        // core's system dir, the download cache and every temp dir go there.
        let data = app
            .internal_data_path()
            .ok_or_else(|| anyhow!("no internal data path"))?;
        let tmp = data.join("tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        // SAFETY: nothing else in the process reads the environment this early.
        unsafe { std::env::set_var("TMPDIR", &tmp) };
        release_io::set_cache_dir(data.join("cache"));
        retro_core::set_system_dir(data.clone());
        unpack_assets(&app, &data)?;

        let event_loop = EventLoop::builder().with_android_app(app).build()?;
        event_loop.run_app(&mut App::new(data.join(DB_ASSET)))?;
        Ok(())
    };
    // The panic hook has logged a panic already.
    if let Ok(Err(e)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
        tracing::error!("{e:#}");
    }
    // Android keeps the process after the activity is gone and would call
    // this again in it, where the logger, winit and the core are all set up.
    std::process::exit(0);
}

#[cfg(test)]
#[path = "tests/player_tests.rs"]
mod tests;
