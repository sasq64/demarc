//! Text file backend: draws ASCII art with a bitmap font into a scrollable
//! frame.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::backend::{Backend, VideoFrame};
use crate::gpu::{Gpu, Submitter, TexturePool, gpu, u32_bytes};
use crate::libretro::{
    RETROK_1, RETROK_4, RETROK_5, RETROK_8, RETROK_DOWN, RETROK_END, RETROK_HOME, RETROK_PAGEDOWN,
    RETROK_PAGEUP, RETROK_TAB, RETROK_UP,
};

const FONTS: [(&[u8], &str); 4] = [
    (include_bytes!("fonts/topaz1200.raw"), "Topaz 1200"),
    (include_bytes!("fonts/topaz.raw"), "Topaz 500"),
    (include_bytes!("fonts/microknight.raw"), "MicroKnight"),
    (include_bytes!("fonts/pot_noodle.raw"), "P0T-NOoDLE"),
];

/// Foreground and background.
const PALETTES: [([u8; 3], [u8; 3]); 4] = [
    ([0x00, 0x00, 0x00], [0xaa, 0xaa, 0xaa]),
    ([0xff, 0xff, 0xff], [0x00, 0x55, 0xaa]),
    ([0xff, 0xff, 0xff], [0x00, 0x00, 0x00]),
    ([0x00, 0x00, 0x00], [0xee, 0xee, 0xee]),
];

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;
const MIN_COLS: usize = 80;
const MAX_COLS: usize = 160;
const ROWS: usize = 32;
const HEIGHT: usize = ROWS * GLYPH_H;
const FRAME_RATE: f64 = 60.0;
/// Pixels per frame while an arrow key is held.
const KEY_SCROLL: f32 = 4.0;
const MINI_SCALE: usize = 8;
/// Border of the dark panel around the minimap.
const MINI_PAD: usize = 4;
/// Share of the remaining distance a glide covers each frame.
const GLIDE: f32 = 0.3;
/// Output pixels per text pixel.
const SCREEN_SCALE: f32 = 2.0;
/// Text pixels per output pixel in the minimap.
const MINI_STEP: f32 = MINI_SCALE as f32 / SCREEN_SCALE;

/// Samples per axis for each minimap pixel.
fn mini_samples() -> usize {
    (MINI_STEP.ceil() as usize).max(1)
}

/// Text pixels to output pixels.
fn scaled(v: usize) -> usize {
    (v as f32 * SCREEN_SCALE + 0.5) as usize
}

/// Output frame size for `cols` columns.
fn out_size(cols: usize, minimap: bool) -> (usize, usize) {
    let text_w = cols * GLYPH_W;
    let mini = if minimap {
        scaled(text_w / MINI_SCALE) + 2 * scaled(MINI_PAD)
    } else {
        0
    };
    (scaled(text_w) + mini, scaled(HEIGHT))
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TextStyle {
    Amiga,
}

impl TextStyle {
    /// Indices into [`FONTS`] and [`PALETTES`].
    fn defaults(self) -> (usize, usize) {
        match self {
            TextStyle::Amiga => (0, 0),
        }
    }
}

/// Lines of ISO-8859-1 bytes, with CRs, tabs and any SAUCE record dealt with.
pub fn parse_text(bytes: &[u8]) -> Vec<Vec<u8>> {
    let end = bytes
        .windows(6)
        .position(|w| w == b"\x1aSAUCE")
        .unwrap_or(bytes.len());
    let bytes = &bytes[..end];
    let latin1: Vec<u8> = match std::str::from_utf8(bytes) {
        Ok(s) if !s.is_ascii() => s
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect(),
        _ => bytes.to_vec(),
    };
    let mut lines: Vec<Vec<u8>> = latin1
        .split(|&b| b == b'\n')
        .map(|line| {
            let mut out = Vec::with_capacity(line.len());
            for &b in line {
                match b {
                    b'\r' | 0x1a => {}
                    b'\t' => out.resize((out.len() / 8 + 1) * 8, b' '),
                    b => out.push(b),
                }
            }
            out
        })
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Scroll position of the text and the minimap, and the minimap's viewport rows.
struct View {
    top: usize,
    mini_top: usize,
    rect: (usize, usize),
}

const SHADER: &str = r#"
struct Params {
    cols: u32, lines: u32, text_w: u32, mini_w: u32,
    top: u32, mini_top: u32, rect_y0: u32, rect_y1: u32,
    fg: u32, bg: u32, dark: u32, mini_h: u32,
}
@group(0) @binding(0) var<storage, read> text: array<u32>;
@group(0) @binding(1) var<storage, read> font: array<u32>;
@group(0) @binding(2) var<uniform> p: Params;
@group(0) @binding(3) var dst: texture_storage_2d<rgba8unorm, write>;

fn byte_at(i: u32) -> u32 {
    return (text[i / 4u] >> ((i % 4u) * 8u)) & 255u;
}

fn lit(px: u32, py: u32) -> u32 {
    let line = py / GLYPH_H;
    let col = px / GLYPH_W;
    if (line >= p.lines || col >= p.cols) {
        return 0u;
    }
    let i = byte_at(line * p.cols + col) * GLYPH_H + py % GLYPH_H;
    let bits = (font[i / 4u] >> ((i % 4u) * 8u)) & 255u;
    return (bits >> (GLYPH_W - 1u - px % GLYPH_W)) & 1u;
}

fn scaled(v: u32) -> u32 {
    return u32(f32(v) * SCREEN_SCALE + 0.5);
}

fn sample(o: u32, i: u32, n: u32, step: f32) -> u32 {
    return u32((f32(o) + (f32(i) + 0.5) / f32(n)) * step);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let size = textureDimensions(dst);
    if (id.x >= size.x || id.y >= size.y) {
        return;
    }
    let fg = unpack4x8unorm(p.fg);
    let bg = unpack4x8unorm(p.bg);
    let text_w = scaled(p.text_w);
    var c: vec4<f32>;
    if (id.x < text_w) {
        let px = sample(id.x, 0u, 1u, TEXT_STEP);
        let py = p.top + sample(id.y, 0u, 1u, TEXT_STEP);
        c = select(bg, fg, lit(px, py) == 1u);
    } else {
        let mini_h = scaled(p.mini_h);
        let x = i32(id.x) - i32(text_w + PAD);
        let y = i32(id.y) - i32(PAD);
        if (x >= 0 && x < i32(scaled(p.mini_w)) && y >= 0 && y < i32(mini_h)) {
            var n = 0u;
            for (var sy = 0u; sy < SAMPLES; sy++) {
                let my = p.mini_top + sample(u32(y), sy, SAMPLES, MINI_STEP);
                for (var sx = 0u; sx < SAMPLES; sx++) {
                    n += lit(sample(u32(x), sx, SAMPLES, MINI_STEP), my);
                }
            }
            c = mix(bg, fg, f32(n) / f32(SAMPLES * SAMPLES));
            if (u32(y) >= scaled(p.rect_y0) && u32(y) < scaled(p.rect_y1)) {
                c = mix(c, fg, 0.2);
            }
        } else if (id.y < mini_h + 2u * PAD) {
            c = unpack4x8unorm(p.dark);
        } else {
            c = bg;
        }
    }
    textureStore(dst, vec2<i32>(id.xy), vec4<f32>(c.rgb, 1.0));
}
"#;

/// Draws the frame with a compute shader into pooled textures.
struct GpuText {
    gpu: Gpu,
    submitter: Submitter,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    text: wgpu::Buffer,
    font: wgpu::Buffer,
    params: wgpu::Buffer,
    size: (u32, u32),
    pool: TexturePool<wgpu::BindGroup>,
}

fn packed(bytes: &[u8]) -> Vec<u32> {
    let mut words: Vec<u32> = bytes
        .chunks(4)
        .map(|c| {
            let mut w = [0; 4];
            w[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(w)
        })
        .collect();
    if words.is_empty() {
        words.push(0);
    }
    words
}

impl GpuText {
    fn new(
        gpu: &Gpu,
        lines: &[Vec<u8>],
        cols: usize,
        font: &[u8],
        size: (u32, u32),
    ) -> Option<Self> {
        use wgpu::util::DeviceExt;
        let device = &gpu.device;
        let mut text = Vec::with_capacity(lines.len() * cols);
        for line in lines {
            let line = &line[..line.len().min(cols)];
            text.extend_from_slice(line);
            text.resize(text.len() + cols - line.len(), b' ');
        }
        let storage = |label, contents: &[u32]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some(label),
                contents: u32_bytes(contents),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            })
        };
        let text = storage("text_chars", &packed(&text));
        let font = storage("text_font", &packed(font));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text_params"),
            size: 48,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let source = format!(
            "const GLYPH_W: u32 = {GLYPH_W}u;\nconst GLYPH_H: u32 = {GLYPH_H}u;\n\
             const SCREEN_SCALE: f32 = {SCREEN_SCALE:?};\n\
             const TEXT_STEP: f32 = {:?};\nconst MINI_STEP: f32 = {MINI_STEP:?};\n\
             const SAMPLES: u32 = {}u;\nconst PAD: u32 = {}u;\n{SHADER}",
            1.0 / SCREEN_SCALE,
            mini_samples(),
            scaled(MINI_PAD),
        );
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("text"),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty,
            count: None,
        };
        let buffer = |ty| wgpu::BindingType::Buffer {
            ty,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let read_only = wgpu::BufferBindingType::Storage { read_only: true };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("text"),
            entries: &[
                entry(0, buffer(read_only)),
                entry(1, buffer(read_only)),
                entry(2, buffer(wgpu::BufferBindingType::Uniform)),
                entry(
                    3,
                    wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                ),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("text"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("text"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Some(Self {
            gpu: gpu.clone(),
            submitter: Submitter::new(gpu.queue.clone())?,
            layout,
            pipeline,
            text,
            font,
            params,
            size,
            pool: TexturePool::default(),
        })
    }

    fn set_font(&self, font: &[u8]) {
        self.gpu
            .queue
            .write_buffer(&self.font, 0, u32_bytes(&packed(font)));
    }

    fn resize(&mut self, size: (u32, u32)) {
        if self.size != size {
            self.size = size;
            self.pool.clear();
        }
    }

    /// Blocks until the last frame has been submitted.
    fn flush(&mut self) {
        while !self.submitter.ready() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn read(&self, texture: &wgpu::Texture) -> Vec<u32> {
        let device = &self.gpu.device;
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("text_readback"),
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.gpu.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| {});
        if device.poll(wgpu::PollType::wait_indefinitely()).is_err() {
            return vec![0; (w * h) as usize];
        }
        let mapped = buffer.get_mapped_range(..);
        mapped
            .chunks_exact(row as usize)
            .flat_map(|line| {
                line[..(w * 4) as usize]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&p| u32::from_ne_bytes(p))
            })
            .collect()
    }

    /// `None` while the last frame is still waiting to be submitted.
    fn draw(&mut self, params: &[u32; 12]) -> Option<Arc<wgpu::Texture>> {
        if !self.submitter.ready() {
            return None;
        }
        self.gpu
            .queue
            .write_buffer(&self.params, 0, u32_bytes(params));
        let device = &self.gpu.device;
        let (width, height) = self.size;
        let make = || {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("text_frame"),
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
            let view = texture.create_view(&Default::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("text"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.text.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.font.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.params.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                ],
            });
            (texture, bind_group)
        };
        let (texture, bind_group) = self.pool.get(make);
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.dispatch_workgroups(width.div_ceil(8), height.div_ceil(8), 1);
        }
        self.submitter.submit(encoder.finish(), Box::new(|| {}));
        Some(texture)
    }
}

pub struct TextEmu {
    lines: Vec<Vec<u8>>,
    cols: usize,
    longest: usize,
    font_index: usize,
    font: &'static [u8],
    fg: u32,
    bg: u32,
    dark: u32,
    minimap: bool,
    scroll: f32,
    target: Option<f32>,
    drawn_at: Option<usize>,
    left_down: bool,
    /// `y` in minimap rows.
    pointer: Option<(f32, f32)>,
    mini_grab: Option<f32>,
    held: f32,
    gpu: Option<GpuText>,
    frame: Option<Arc<wgpu::Texture>>,
    /// The frame read back, while frames have to come with their pixels.
    pixels: Arc<Vec<u32>>,
    cpu_frames: bool,
}

fn pack([r, g, b]: [u8; 3]) -> u32 {
    u32::from_ne_bytes([r, g, b, 255])
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let (a, b) = (a.to_ne_bytes(), b.to_ne_bytes());
    u32::from_ne_bytes(std::array::from_fn(|i| {
        (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8
    }))
}

impl TextEmu {
    pub fn new(path: &Path, style: TextStyle) -> Result<Self> {
        let gpu = gpu().context("text needs a GPU")?;
        Ok(Self::with_gpu(&fs::read(path)?, style, Some(gpu)))
    }

    fn with_gpu(bytes: &[u8], style: TextStyle, gpu: Option<&Gpu>) -> Self {
        let lines = parse_text(bytes);
        let longest = lines.iter().map(Vec::len).max().unwrap_or(0);
        let cols = longest.clamp(MIN_COLS, MAX_COLS);
        let (font_index, palette) = style.defaults();
        let font = FONTS[font_index].0;
        let (width, height) = out_size(cols, true);
        let gpu =
            gpu.and_then(|g| GpuText::new(g, &lines, cols, font, (width as u32, height as u32)));
        let mut emu = Self {
            lines,
            cols,
            longest,
            font_index,
            font,
            fg: 0,
            bg: 0,
            dark: 0,
            minimap: true,
            scroll: 0.0,
            target: None,
            drawn_at: None,
            left_down: false,
            pointer: None,
            mini_grab: None,
            held: 0.0,
            gpu,
            frame: None,
            pixels: Arc::default(),
            cpu_frames: false,
        };
        emu.set_palette(palette);
        emu
    }

    fn text_width(&self) -> usize {
        self.cols * GLYPH_W
    }

    fn mini_width(&self) -> usize {
        self.text_width() / MINI_SCALE
    }

    fn width(&self) -> usize {
        if self.minimap {
            self.text_width() + self.mini_width() + 2 * MINI_PAD
        } else {
            self.text_width()
        }
    }

    fn out_size(&self) -> (usize, usize) {
        out_size(self.cols, self.minimap)
    }

    fn set_font(&mut self, index: usize) {
        self.font_index = index;
        self.font = FONTS[index].0;
        if let Some(gpu) = &self.gpu {
            gpu.set_font(self.font);
        }
        self.redraw();
    }

    fn set_palette(&mut self, index: usize) {
        let (fg, bg) = PALETTES[index];
        let (fg, bg) = (pack(fg), pack(bg));
        self.dark = mix(bg, fg, 0.6);
        (self.fg, self.bg) = (fg, bg);
        self.redraw();
    }

    fn redraw(&mut self) {
        self.drawn_at = None;
        self.render();
    }

    /// Height of the minimap's text, inside its panel.
    fn mini_height(&self) -> usize {
        (self.lines.len() * GLYPH_H / MINI_SCALE).min(HEIGHT - 2 * MINI_PAD)
    }

    /// Viewport rectangle top and height in the minimap, and the distance it
    /// travels over the whole scroll range.
    fn mini_geom(&self) -> (f32, f32, f32) {
        let s = MINI_SCALE as f32;
        let total = (self.lines.len() * GLYPH_H) as f32 / s;
        let rect_h = HEIGHT as f32 / s;
        let track = (total.min(self.mini_height() as f32) - rect_h).max(0.0);
        let max = self.max_scroll();
        let rect_y = if max > 0.0 {
            self.scroll * track / max
        } else {
            0.0
        };
        (rect_y, rect_h, track)
    }

    /// `py` is in minimap rows.
    fn drag_minimap(&mut self, py: f32, grab: f32) {
        let (_, _, track) = self.mini_geom();
        if track > 0.0 {
            self.glide_to((py - grab) * self.max_scroll() / track);
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.lines.len() * GLYPH_H).saturating_sub(HEIGHT) as f32
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll as usize
    }

    fn scroll_to(&mut self, y: f32) {
        self.target = None;
        self.scroll = y.clamp(0.0, self.max_scroll());
        self.render();
    }

    fn scroll_by(&mut self, dy: f32) {
        self.scroll_to(self.scroll + dy);
    }

    /// Scroll to `y` over the next frames.
    fn glide_to(&mut self, y: f32) {
        self.target = Some(y.clamp(0.0, self.max_scroll()));
    }

    fn glide(&mut self) {
        let Some(target) = self.target else {
            return;
        };
        let d = target - self.scroll;
        if d.abs() <= 1.0 {
            self.scroll = target;
            self.target = None;
        } else {
            self.scroll += d.signum() * (d.abs() * GLIDE).max(1.0);
        }
        self.render();
    }

    fn view(&self) -> View {
        let (rect_y, rect_h, _) = self.mini_geom();
        View {
            top: self.scroll as usize,
            mini_top: (self.scroll - rect_y * MINI_SCALE as f32).max(0.0) as usize,
            rect: (
                rect_y as usize,
                ((rect_y + rect_h) as usize).min(self.mini_height()),
            ),
        }
    }

    fn render(&mut self) {
        let view = self.view();
        if self.drawn_at == Some(view.top) {
            return;
        }
        let (width, height) = self.out_size();
        let mini_w = if self.minimap {
            self.mini_width() as u32
        } else {
            0
        };
        let params = [
            self.cols as u32,
            self.lines.len() as u32,
            (self.cols * GLYPH_W) as u32,
            mini_w,
            view.top as u32,
            view.mini_top as u32,
            view.rect.0 as u32,
            view.rect.1 as u32,
            self.fg,
            self.bg,
            self.dark,
            self.mini_height() as u32,
        ];
        let Some(gpu) = &mut self.gpu else {
            return;
        };
        gpu.resize((width as u32, height as u32));
        let Some(texture) = gpu.draw(&params) else {
            return;
        };
        if self.cpu_frames {
            gpu.flush();
            self.pixels = Arc::new(gpu.read(&texture));
        }
        self.frame = Some(texture);
        self.drawn_at = Some(view.top);
    }
}

impl Backend for TextEmu {
    fn run(&mut self) -> bool {
        if self.held != 0.0 {
            self.scroll_by(self.held);
        }
        self.glide();
        self.render();
        true
    }

    fn press_key(&mut self, code: u32, down: bool, _mods: u16) {
        let page = HEIGHT as f32;
        match code {
            RETROK_UP => self.held = if down { -KEY_SCROLL } else { 0.0 },
            RETROK_DOWN => self.held = if down { KEY_SCROLL } else { 0.0 },
            RETROK_PAGEUP if down => self.glide_to(self.target.unwrap_or(self.scroll) - page),
            RETROK_PAGEDOWN if down => self.glide_to(self.target.unwrap_or(self.scroll) + page),
            RETROK_1..=RETROK_4 if down => self.set_font((code - RETROK_1) as usize),
            RETROK_5..=RETROK_8 if down => self.set_palette((code - RETROK_5) as usize),
            RETROK_TAB if down => {
                self.minimap = !self.minimap;
                self.redraw();
            }
            RETROK_HOME if down => self.scroll_by(-self.max_scroll()),
            RETROK_END if down => self.scroll_by(self.max_scroll()),
            _ => {}
        }
    }

    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}

    fn set_mouse_position(&mut self, x: f32, y: f32) {
        let py = y * HEIGHT as f32 - MINI_PAD as f32;
        let last = self.pointer.replace((x * self.width() as f32, py));
        if let Some(grab) = self.mini_grab {
            self.drag_minimap(py, grab);
        } else if self.left_down
            && let Some((_, last_y)) = last
        {
            self.scroll_by(last_y - py);
        }
    }

    fn wants_pointer(&self) -> bool {
        true
    }

    fn set_mouse_buttons(&mut self, left: bool, _right: bool, _middle: bool) {
        if left
            && !self.left_down
            && let Some((px, py)) = self.pointer
            && self.minimap
            && px >= self.text_width() as f32
        {
            let (rect_y, rect_h, _) = self.mini_geom();
            let inside = (rect_y..rect_y + rect_h).contains(&py);
            let grab = if inside { py - rect_y } else { rect_h / 2.0 };
            self.mini_grab = Some(grab);
            self.drag_minimap(py, grab);
        }
        if !left {
            self.mini_grab = None;
        }
        self.left_down = left;
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        let Some(texture) = &self.frame else {
            let (width, height) = self.out_size();
            return f(width, height, &vec![0; width * height]);
        };
        let size = (texture.width() as usize, texture.height() as usize);
        if self.cpu_frames {
            f(size.0, size.1, &self.pixels);
        } else if let Some(gpu) = &self.gpu {
            f(size.0, size.1, &gpu.read(texture));
        }
    }

    fn frame(&self) -> VideoFrame {
        let Some(texture) = &self.frame else {
            return VideoFrame::default();
        };
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        if self.cpu_frames {
            VideoFrame {
                width,
                height,
                pixels: Arc::clone(&self.pixels),
                gpu: None,
            }
        } else {
            VideoFrame {
                width,
                height,
                pixels: Arc::default(),
                gpu: Some(texture.clone()),
            }
        }
    }

    fn set_cpu_frames(&mut self, cpu: bool) {
        if self.cpu_frames == cpu {
            return;
        }
        self.cpu_frames = cpu;
        self.redraw();
    }

    fn with_audio(&mut self, _f: &mut dyn FnMut(&[f32])) {}

    fn get_frame_size(&self) -> (usize, usize) {
        self.out_size()
    }

    fn aspect_ratio(&self) -> f32 {
        let (width, height) = self.out_size();
        width as f32 / height as f32
    }

    fn sample_rate(&self) -> f64 {
        0.0
    }

    fn fps(&self) -> f64 {
        FRAME_RATE
    }

    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        1
    }
    fn reset(&mut self) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn skip_frames(&mut self, _frames: u32) {}

    fn get_info(&self) -> Option<String> {
        let font = FONTS[self.font_index].1;
        Some(format!(
            "ASCII {}x{} ({font})",
            self.longest,
            self.lines.len()
        ))
    }
}

#[cfg(test)]
#[path = "tests/text_renderer_tests.rs"]
mod tests;
