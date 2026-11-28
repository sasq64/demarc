//! Text file backend: draws ASCII art with a bitmap font into a scrollable
//! frame.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::backend::{Backend, VideoFrame};
use crate::libretro::{
    RETROK_DOWN, RETROK_END, RETROK_HOME, RETROK_PAGEDOWN, RETROK_PAGEUP, RETROK_UP,
};

const TOPAZ: &[u8] = include_bytes!("fonts/topaz1200.raw");

const GLYPH_W: usize = 8;
const GLYPH_H: usize = 16;
const MIN_COLS: usize = 80;
const MAX_COLS: usize = 160;
const ROWS: usize = 32;
const HEIGHT: usize = ROWS * GLYPH_H;
const FRAME_RATE: f64 = 60.0;
/// Pixels per frame while an arrow key is held.
const KEY_SCROLL: f32 = 4.0;
const MINI_SCALE: usize = 4;

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum TextStyle {
    Amiga,
}

impl TextStyle {
    fn colors(self) -> ([u8; 3], [u8; 3]) {
        match self {
            TextStyle::Amiga => ([0x00, 0x00, 0x00], [0xaa, 0xaa, 0xaa]),
        }
    }

    fn font(self) -> (&'static [u8], &'static str) {
        match self {
            TextStyle::Amiga => (TOPAZ, "Topaz"),
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

pub struct TextEmu {
    lines: Vec<Vec<u8>>,
    cols: usize,
    font: &'static [u8],
    fg: u32,
    bg: u32,
    mini_palette: Vec<u32>,
    scroll: f32,
    drawn_at: Option<usize>,
    left_down: bool,
    pointer: Option<(f32, f32)>,
    mini_grab: Option<f32>,
    held: f32,
    frame: Arc<Vec<u32>>,
    info: String,
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

/// Draws the text region `w` x `HEIGHT` at column `x0`, each pixel covering
/// `scale` x `scale` source pixels from `top`; `palette[n]` is the color for
/// `n` lit source pixels.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    lines: &[Vec<u8>],
    font: &[u8],
    palette: &[u32],
    frame: &mut [u32],
    stride: usize,
    x0: usize,
    w: usize,
    top: usize,
    scale: usize,
) {
    for (y, row) in frame.chunks_exact_mut(stride).enumerate() {
        for (x, p) in row[x0..x0 + w].iter_mut().enumerate() {
            let mut lit = 0;
            for sy in 0..scale {
                let py = top + y * scale + sy;
                let Some(line) = lines.get(py / GLYPH_H) else {
                    break;
                };
                for sx in 0..scale {
                    let px = x * scale + sx;
                    if let Some(&ch) = line.get(px / GLYPH_W) {
                        let bits = font[ch as usize * GLYPH_H + py % GLYPH_H];
                        lit += usize::from(bits & (0x80 >> (px % GLYPH_W)) != 0);
                    }
                }
            }
            *p = palette[lit];
        }
    }
}

impl TextEmu {
    pub fn new(path: &Path, style: TextStyle) -> Result<Self> {
        Ok(Self::from_bytes(&fs::read(path)?, style))
    }

    pub fn from_bytes(bytes: &[u8], style: TextStyle) -> Self {
        let lines = parse_text(bytes);
        let longest = lines.iter().map(Vec::len).max().unwrap_or(0);
        let cols = longest.clamp(MIN_COLS, MAX_COLS);
        let (fg, bg) = style.colors();
        let (font, font_name) = style.font();
        let info = format!("ASCII {longest}x{} ({font_name})", lines.len());
        let (fg, bg) = (pack(fg), pack(bg));
        let n = MINI_SCALE * MINI_SCALE;
        let mini_palette = (0..=n).map(|i| mix(bg, fg, i as f32 / n as f32)).collect();
        let width = cols * GLYPH_W + cols * GLYPH_W / MINI_SCALE;
        let mut emu = Self {
            lines,
            cols,
            font,
            fg,
            bg,
            mini_palette,
            scroll: 0.0,
            drawn_at: None,
            left_down: false,
            pointer: None,
            mini_grab: None,
            held: 0.0,
            frame: Arc::new(vec![0; width * HEIGHT]),
            info,
        };
        emu.render();
        emu
    }

    fn text_width(&self) -> usize {
        self.cols * GLYPH_W
    }

    fn mini_width(&self) -> usize {
        self.text_width() / MINI_SCALE
    }

    fn width(&self) -> usize {
        self.text_width() + self.mini_width()
    }

    /// Viewport rectangle top and height in the minimap, and the distance it
    /// travels over the whole scroll range.
    fn mini_geom(&self) -> (f32, f32, f32) {
        let s = MINI_SCALE as f32;
        let total = (self.lines.len() * GLYPH_H) as f32 / s;
        let rect_h = HEIGHT as f32 / s;
        let track = (total.min(HEIGHT as f32) - rect_h).max(0.0);
        let max = self.max_scroll();
        let rect_y = if max > 0.0 {
            self.scroll * track / max
        } else {
            0.0
        };
        (rect_y, rect_h, track)
    }

    fn drag_minimap(&mut self, py: f32, grab: f32) {
        let (_, _, track) = self.mini_geom();
        if track > 0.0 {
            self.scroll_to((py - grab) * self.max_scroll() / track);
        }
    }

    fn max_scroll(&self) -> f32 {
        (self.lines.len() * GLYPH_H).saturating_sub(HEIGHT) as f32
    }

    pub fn scroll_offset(&self) -> usize {
        self.scroll as usize
    }

    fn scroll_to(&mut self, y: f32) {
        self.scroll = y.clamp(0.0, self.max_scroll());
        self.render();
    }

    fn scroll_by(&mut self, dy: f32) {
        self.scroll_to(self.scroll + dy);
    }

    fn render(&mut self) {
        let top = self.scroll as usize;
        if self.drawn_at == Some(top) {
            return;
        }
        self.drawn_at = Some(top);
        let (width, text_w, mini_w) = (self.width(), self.text_width(), self.mini_width());
        let (rect_y, rect_h, _) = self.mini_geom();
        let mini_top = (self.scroll - rect_y * MINI_SCALE as f32).max(0.0) as usize;
        let (fg, lines, font) = (self.fg, &self.lines, self.font);
        let frame = Arc::make_mut(&mut self.frame);
        draw_text(lines, font, &[self.bg, fg], frame, width, 0, text_w, top, 1);
        draw_text(
            lines,
            font,
            &self.mini_palette,
            frame,
            width,
            text_w,
            mini_w,
            mini_top,
            MINI_SCALE,
        );
        let (y0, y1) = (rect_y as usize, (rect_y + rect_h) as usize);
        for row in frame.chunks_exact_mut(width).take(y1).skip(y0) {
            for p in &mut row[text_w..] {
                *p = mix(*p, fg, 0.2);
            }
        }
    }
}

impl Backend for TextEmu {
    fn run(&mut self) -> bool {
        if self.held != 0.0 {
            self.scroll_by(self.held);
        }
        true
    }

    fn press_key(&mut self, code: u32, down: bool, _mods: u16) {
        let page = HEIGHT as f32;
        match code {
            RETROK_UP => self.held = if down { -KEY_SCROLL } else { 0.0 },
            RETROK_DOWN => self.held = if down { KEY_SCROLL } else { 0.0 },
            RETROK_PAGEUP if down => self.scroll_by(-page),
            RETROK_PAGEDOWN if down => self.scroll_by(page),
            RETROK_HOME if down => self.scroll_by(-self.max_scroll()),
            RETROK_END if down => self.scroll_by(self.max_scroll()),
            _ => {}
        }
    }

    fn add_mouse_motion(&mut self, _dx: f32, dy: f32) {
        if self.left_down && self.mini_grab.is_none() {
            self.scroll_by(-dy);
        }
    }

    fn set_mouse_position(&mut self, x: f32, y: f32) {
        let py = y * HEIGHT as f32;
        self.pointer = Some((x * self.width() as f32, py));
        if let Some(grab) = self.mini_grab {
            self.drag_minimap(py, grab);
        }
    }

    fn set_mouse_buttons(&mut self, left: bool, _right: bool, _middle: bool) {
        if left
            && !self.left_down
            && let Some((px, py)) = self.pointer
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
        f(self.width(), HEIGHT, &self.frame);
    }

    fn frame(&self) -> VideoFrame {
        VideoFrame {
            width: self.width(),
            height: HEIGHT,
            pixels: Arc::clone(&self.frame),
            gpu: None,
        }
    }

    fn with_audio(&mut self, _f: &mut dyn FnMut(&[f32])) {}

    fn get_frame_size(&self) -> (usize, usize) {
        (self.width(), HEIGHT)
    }

    fn aspect_ratio(&self) -> f32 {
        self.width() as f32 / HEIGHT as f32
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
        Some(self.info.clone())
    }
}

#[cfg(test)]
#[path = "tests/text_renderer_tests.rs"]
mod tests;
