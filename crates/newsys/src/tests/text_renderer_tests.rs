use super::*;

fn tall_text(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .into_bytes()
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
    let emu = TextEmu::from_bytes(b"hi", TextStyle::Amiga);
    assert_eq!(emu.get_frame_size(), (MIN_COLS * GLYPH_W * 5 / 4, HEIGHT));
    let wide = vec![b'x'; 100];
    let emu = TextEmu::from_bytes(&wide, TextStyle::Amiga);
    assert_eq!(emu.get_frame_size(), (100 * GLYPH_W * 5 / 4, HEIGHT));
}

#[test]
fn draws_glyphs_on_background() {
    let emu = TextEmu::from_bytes(&[0xdb], TextStyle::Amiga);
    let frame = emu.frame();
    let (fg, bg) = (emu.fg, emu.bg);
    let w = frame.width;
    let lit = (0..GLYPH_H).any(|y| (0..GLYPH_W).any(|x| frame.pixels[y * w + x] == fg));
    assert!(lit);
    assert_eq!(frame.pixels[HEIGHT * w - 1], bg);
}

#[test]
fn drag_scrolls_only_with_button_down() {
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    emu.add_mouse_motion(0.0, -50.0);
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_buttons(true, false, false);
    emu.add_mouse_motion(0.0, -50.0);
    assert_eq!(emu.scroll_offset(), 50);
    emu.add_mouse_motion(0.0, 500.0);
    assert_eq!(emu.scroll_offset(), 0);
}

#[test]
fn keys_scroll_and_clamp() {
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    emu.press_key(RETROK_PAGEDOWN, true, 0);
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
    let mut emu = TextEmu::from_bytes(b"one line", TextStyle::Amiga);
    emu.press_key(RETROK_PAGEDOWN, true, 0);
    assert_eq!(emu.scroll_offset(), 0);
}

fn click(emu: &mut TextEmu, x: f32, y: f32) {
    emu.set_mouse_position(x, y);
    emu.set_mouse_buttons(true, false, false);
}

#[test]
fn minimap_draws_blended_glyphs_and_viewport() {
    let text: Vec<u8> = (0..200).flat_map(|_| [0xdb, b'\n']).collect();
    let emu = TextEmu::from_bytes(&text, TextStyle::Amiga);
    let frame = emu.frame();
    let (w, tw) = (frame.width, emu.text_width());
    let in_rect = frame.pixels[tw];
    assert_ne!(in_rect, emu.bg);
    assert_ne!(in_rect, emu.mini_palette[0]);
    let below = frame.pixels[(HEIGHT / MINI_SCALE + 1) * w + tw];
    assert_ne!(in_rect, below);
    assert_ne!(below, emu.bg);
}

#[test]
fn minimap_click_jumps_and_drag_scrolls() {
    // 100 lines: minimap is 400px tall, viewport rect 128px, track 272px.
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    let max = emu.max_scroll();
    let y = |py: f32| py / HEIGHT as f32;
    click(&mut emu, 0.99, y(200.0));
    assert_eq!(emu.scroll_offset(), ((200.0 - 64.0) * max / 272.0) as usize);
    emu.set_mouse_position(0.99, y(500.0));
    assert_eq!(emu.scroll_offset(), max as usize);
    emu.set_mouse_buttons(false, false, false);

    emu.press_key(RETROK_HOME, true, 0);
    click(&mut emu, 0.99, y(10.0));
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_position(0.99, y(10.0 + 136.0));
    assert_eq!(emu.scroll_offset(), (max / 2.0) as usize);
    emu.add_mouse_motion(0.0, -50.0);
    assert_eq!(emu.scroll_offset(), (max / 2.0) as usize);
}

#[test]
fn click_in_text_does_not_jump() {
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    click(&mut emu, 0.5, 0.9);
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_position(0.5, 0.1);
    assert_eq!(emu.scroll_offset(), 0);
}
