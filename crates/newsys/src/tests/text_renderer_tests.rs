use super::*;
use crate::libretro::{RETROK_2, RETROK_3, RETROK_6, RETROK_7};

fn tall_text(lines: usize) -> Vec<u8> {
    (0..lines)
        .map(|i| format!("line {i}\n"))
        .collect::<String>()
        .into_bytes()
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
    let emu = TextEmu::from_bytes(b"hi", TextStyle::Amiga);
    let pad = 2 * MINI_PAD;
    assert_eq!(emu.get_frame_size(), (MIN_COLS * GLYPH_W * 5 / 4 + pad, HEIGHT));
    let wide = vec![b'x'; 100];
    let emu = TextEmu::from_bytes(&wide, TextStyle::Amiga);
    assert_eq!(emu.get_frame_size(), (100 * GLYPH_W * 5 / 4 + pad, HEIGHT));
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
    let mut emu = TextEmu::from_bytes(b"one line", TextStyle::Amiga);
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
    let text: Vec<u8> = (0..200).flat_map(|_| [0xdb, b'\n']).collect();
    let emu = TextEmu::from_bytes(&text, TextStyle::Amiga);
    let frame = emu.frame();
    let (w, x) = (frame.width, emu.text_width() + MINI_PAD);
    assert_eq!(frame.pixels[x - 1], emu.dark);
    let in_rect = frame.pixels[MINI_PAD * w + x];
    assert_ne!(in_rect, emu.bg);
    assert_ne!(in_rect, emu.mini_palette[0]);
    let below = frame.pixels[(HEIGHT / MINI_SCALE + 1 + MINI_PAD) * w + x];
    assert_ne!(in_rect, below);
    assert_ne!(below, emu.bg);
    assert_eq!(frame.pixels[(HEIGHT - 1) * w + w - 1], emu.dark);
}

#[test]
fn minimap_click_jumps_and_drag_scrolls() {
    // 100 lines: minimap is 400px tall, viewport rect 128px, track 272px.
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    let max = emu.max_scroll();
    let y = |py: f32| (py + MINI_PAD as f32) / HEIGHT as f32;
    click(&mut emu, 0.99, y(200.0));
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), ((200.0 - 64.0) * max / 272.0) as usize);
    emu.set_mouse_position(0.99, y(500.0));
    settle(&mut emu);
    assert_eq!(emu.scroll_offset(), max as usize);
    emu.set_mouse_buttons(false, false, false);

    emu.press_key(RETROK_HOME, true, 0);
    click(&mut emu, 0.99, y(10.0));
    assert_eq!(emu.scroll_offset(), 0);
    emu.set_mouse_position(0.99, y(10.0 + 136.0));
    settle(&mut emu);
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

fn read_texture(gpu: &Gpu, texture: &wgpu::Texture) -> Vec<u32> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (row * h) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
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
    gpu.queue.submit([encoder.finish()]);
    buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    let mapped = buffer.get_mapped_range(..);
    mapped
        .chunks_exact(row as usize)
        .flat_map(|line| {
            line[..(w * 4) as usize]
                .chunks_exact(4)
                .map(|p| u32::from_ne_bytes(p.try_into().unwrap()))
        })
        .collect()
}

#[test]
fn gpu_frame_matches_cpu() {
    let Some(gpu) = test_gpu() else {
        return;
    };
    let mut text: Vec<u8> = (0..200)
        .flat_map(|i| format!("{i:3} \u{2588}\u{2593}\u{2591} {}\n", "#.%@".repeat(i % 50)).into_bytes())
        .collect();
    text.extend((32..=255u8).filter(|&c| c != b'\n'));
    let mut emu = TextEmu::with_gpu(&text, TextStyle::Amiga, Some(&gpu));
    for scroll in [0.0, 333.0, 1e9] {
        emu.scroll_to(scroll);
        assert_gpu_matches_cpu(&gpu, &mut emu);
    }
    for key in [RETROK_3, RETROK_6, RETROK_TAB] {
        emu.press_key(key, true, 0);
        assert_gpu_matches_cpu(&gpu, &mut emu);
    }
}

fn assert_gpu_matches_cpu(gpu: &Gpu, emu: &mut TextEmu) {
    while !emu.gpu.as_mut().unwrap().submitter.ready() {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let frame = emu.frame();
    let texture = frame.gpu.expect("a GPU frame");
    let texture = texture.downcast_ref::<wgpu::Texture>().unwrap();
    assert_eq!(texture.width() as usize, emu.width());
    let got = read_texture(gpu, texture);
    let mut want = vec![0; emu.width() * HEIGHT];
    emu.draw(&mut want, &emu.view());
    for (i, (g, w)) in got.iter().zip(&want).enumerate() {
        let close = g
            .to_ne_bytes()
            .iter()
            .zip(w.to_ne_bytes())
            .all(|(a, b)| a.abs_diff(b) <= 1);
        assert!(close, "pixel {i}: {g:08x} != {w:08x}");
    }
}

#[test]
fn keys_pick_font_palette_and_minimap() {
    let mut emu = TextEmu::from_bytes(&tall_text(100), TextStyle::Amiga);
    let width = emu.width();
    emu.press_key(RETROK_2, true, 0);
    assert_eq!(emu.font, FONTS[1].0);
    emu.press_key(RETROK_7, true, 0);
    assert_eq!((emu.fg, emu.bg), (pack([0xff; 3]), pack([0; 3])));
    assert_eq!(emu.frame().pixels[emu.width() - 1], emu.dark);
    emu.press_key(RETROK_TAB, true, 0);
    assert_eq!(emu.width(), emu.text_width());
    assert_eq!(emu.frame().pixels.len(), emu.text_width() * HEIGHT);
    emu.press_key(RETROK_TAB, true, 0);
    assert_eq!(emu.width(), width);
}
