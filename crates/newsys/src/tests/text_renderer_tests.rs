use super::*;
use crate::libretro::{RETROK_2, RETROK_3, RETROK_7, RETROK_8};

fn tall_text(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .into_bytes()
}

fn text_emu(bytes: &[u8]) -> TextEmu {
    TextEmu::with_gpu(bytes, TextStyle::Amiga, None)
}

/// `None` without a GPU adapter. Leaked, as tearing the device down while the
/// process exits crashes the driver.
fn gpu_emu(bytes: &[u8]) -> Option<&'static mut TextEmu> {
    let gpu = test_gpu()?;
    Some(Box::leak(Box::new(TextEmu::with_gpu(
        bytes,
        TextStyle::Amiga,
        Some(&gpu),
    ))))
}

/// The current frame's size and pixels.
fn pixels(emu: &mut TextEmu) -> (usize, usize, Vec<u32>) {
    emu.gpu.as_mut().unwrap().flush();
    let mut out = (0, 0, Vec::new());
    emu.with_frame(&mut |w, h, p| out = (w, h, p.to_vec()));
    out
}

fn settle(emu: &mut TextEmu) {
    for _ in 0..100 {
        emu.run();
    }
    assert!(emu.target.is_none());
}

#[test]
fn parses_crlf_tabs_and_sauce() {
    let lines = parse_text(b"a\r\n\tb\r\n\n\x1aSAUCE00junk");
    assert_eq!(lines, vec![b"a".to_vec(), b"        b".to_vec()]);
}

#[test]
fn utf8_becomes_latin1() {
    assert_eq!(
        parse_text("\u{e5}\u{2588}".as_bytes()),
        vec![vec![0xe5, b'?']]
    );
}

#[test]
fn frame_size_follows_longest_line() {
    let size = |cols: usize| {
        let w = cols * GLYPH_W;
        (
            scaled(w) + scaled(w / MINI_SCALE) + 2 * scaled(MINI_PAD),
            scaled(HEIGHT),
        )
    };
    let emu = text_emu(b"hi");
    assert_eq!(emu.get_frame_size(), size(MIN_COLS));
    let wide = vec![b'x'; 100];
    let emu = text_emu(&wide);
    let expected = if ALWAYS_80_COLUMNS { 80 } else { 100 };
    assert_eq!(emu.get_frame_size(), size(expected));
    let padded = [b"hi".as_slice(), &[b' '; 100]].concat();
    assert_eq!(text_emu(&padded).get_frame_size(), size(MIN_COLS));
}

#[test]
fn draws_glyphs_on_background() {
    let Some(emu) = gpu_emu(&[0xdb]) else {
        return;
    };
    let (w, h, frame) = pixels(emu);
    let lit = (0..GLYPH_H).any(|y| (0..GLYPH_W).any(|x| frame[y * w + x] == emu.fg));
    assert!(lit);
    assert_eq!(frame[h * w - 1], emu.bg);
}

#[test]
fn drag_scrolls_only_with_button_down() {
    let mut emu = text_emu(&tall_text(100));
    let y = |py: f32| py / HEIGHT as f32;
    emu.set_mouse_position(0.5, y(300.0));
    emu.set_mouse_position(0.5, y(250.0));
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_buttons(true, false, false);
    emu.set_mouse_position(0.5, y(200.0));
    assert_eq!(emu.scroll_offset(), 50);
    emu.set_mouse_position(0.5, y(500.0));
    assert_eq!(emu.scroll_offset(), 0);
}

#[test]
fn keys_scroll_and_clamp() {
    let mut emu = text_emu(&tall_text(100));
    emu.press_key(RETROK_PAGEDOWN, true, 0);
    emu.run();
    assert!(emu.scroll_offset() > 0 && emu.scroll_offset() < HEIGHT);
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), HEIGHT);
    emu.press_key(RETROK_END, true, 0);
    assert_eq!(emu.scroll_offset(), 100 * GLYPH_H - HEIGHT);
    emu.press_key(RETROK_DOWN, true, 0);
    emu.run();
    assert_eq!(emu.scroll_offset(), 100 * GLYPH_H - HEIGHT);
    emu.press_key(RETROK_DOWN, false, 0);
    emu.press_key(RETROK_HOME, true, 0);
    emu.press_key(RETROK_UP, true, 0);
    emu.run();
    assert_eq!(emu.scroll_offset(), 0);
}

#[test]
fn short_text_does_not_scroll() {
    let mut emu = text_emu(b"one line");
    emu.press_key(RETROK_PAGEDOWN, true, 0);
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), 0);
}

fn click(emu: &mut TextEmu, x: f32, y: f32) {
    emu.set_mouse_position(x, y);
    emu.set_mouse_buttons(true, false, false);
}

#[test]
fn minimap_draws_blended_glyphs_and_viewport() {
    let text: Vec<u8> = (0..400).flat_map(|_| [0xdb, b'\n']).collect();
    let Some(emu) = gpu_emu(&text) else {
        return;
    };
    let (w, h, frame) = pixels(emu);
    let pad = scaled(MINI_PAD);
    let x = scaled(emu.text_width()) + pad;
    assert_eq!(frame[x - 1], emu.dark);
    let in_rect = frame[pad * w + x];
    assert_ne!(in_rect, emu.bg);
    let below = frame[(scaled(HEIGHT / MINI_SCALE) + 2 + pad) * w + x];
    assert_ne!(in_rect, below);
    assert_ne!(below, emu.bg);
    assert_eq!(frame[(h - 1) * w + w - 1], emu.dark);
}

#[test]
fn minimap_click_jumps_and_drag_scrolls() {
    let mut emu = text_emu(&tall_text(100));
    let max = emu.max_scroll();
    let (_, rect_h, track) = emu.mini_geom();
    let y = |py: f32| (py + MINI_PAD as f32) / HEIGHT as f32;
    click(&mut emu, 0.99, y(rect_h / 2.0 + track / 2.0));
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), (max / 2.0) as usize);
    emu.set_mouse_position(0.99, y(HEIGHT as f32));
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), max as usize);
    emu.set_mouse_buttons(false, false, false);

    emu.press_key(RETROK_HOME, true, 0);
    click(&mut emu, 0.99, y(10.0));
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_position(0.99, y(10.0 + track / 2.0));
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), (max / 2.0) as usize);
}

#[test]
fn click_in_text_does_not_jump() {
    let mut emu = text_emu(&tall_text(100));
    click(&mut emu, 0.5, 0.9);
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_position(0.5, 0.8);
    assert_eq!(emu.scroll_offset(), (0.1 * HEIGHT as f32).round() as usize);
}

fn test_gpu() -> Option<Gpu> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("no GPU adapter, skipping");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).ok()?;
    Some(Gpu { device, queue })
}

#[test]
fn keys_pick_font_palette_and_minimap() {
    let Some(emu) = gpu_emu(&tall_text(100)) else {
        return;
    };
    let width = emu.width();
    emu.press_key(RETROK_2, true, 0);
    assert_eq!(emu.font, FONTS[1].0);
    emu.press_key(RETROK_7, true, 0);
    assert_eq!((emu.fg, emu.bg), (pack([0xff; 3]), pack([0; 3])));
    let (w, _, frame) = pixels(emu);
    assert_eq!(frame[w - 1], emu.dark);
    emu.press_key(RETROK_TAB, true, 0);
    emu.run();
    assert_eq!(emu.width(), emu.text_width());
    let (w, h, frame) = pixels(emu);
    assert_eq!((w, h), (scaled(emu.text_width()), scaled(HEIGHT)));
    assert_eq!(frame.len(), w * h);
    emu.press_key(RETROK_TAB, true, 0);
    assert_eq!(emu.width(), width);
}

#[test]
fn next_file_keeps_font_palette_and_minimap() {
    let mut emu = text_emu(b"hi");
    emu.press_key(RETROK_3, true, 0);
    emu.press_key(RETROK_8, true, 0);
    emu.press_key(RETROK_TAB, true, 0);
    let next = text_emu(b"there");
    assert_eq!((next.font_index, next.palette, next.minimap), (2, 3, false));
}

#[test]
fn ansi_colors_and_wraps_at_80() {
    let mut text = b"\x1b[1;31ma\x1b[0;5;44mb\x1b[mc\r\n".to_vec();
    text.extend_from_slice(&[b'x'; 80]);
    text.extend_from_slice(b"\r\nd");
    let (lines, attrs) = parse_ansi(&text).unwrap();
    assert_eq!(lines[0], b"abc");
    assert_eq!(attrs[0], [0x09, 0xc7, 0x07]);
    assert_eq!(lines[1].len(), 80);
    assert_eq!(lines[2], b"");
    assert_eq!(lines[3], b"d");
    assert!(parse_ansi(b"plain").is_none());
}

#[test]
fn ansi_draws_cell_colors() {
    let Some(emu) = gpu_emu(b"\x1b[44m \x1b[0m") else {
        return;
    };
    assert_eq!(emu.cols, ANSI_COLS);
    let (w, h, frame) = pixels(emu);
    assert_eq!(frame[0], pack(ANSI_PALETTE[4]));
    assert_eq!(frame[h * w - 1], pack(ANSI_PALETTE[0]));
}
