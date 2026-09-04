use super::*;

/// Paths here are rooted at the crate directory rather than left relative:
/// a conversion running in another test switches the process-wide working
/// directory for its duration (see `cbmconvert::CwdGuard`).
fn root(rel: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// [`get_info`](Backend::get_info) names the format the file turned out to
/// be, the size it is displayed at (aspect correction included) and its
/// colour mode — for every decoder the image path can end up in.
#[test]
fn get_info_names_the_format() {
    let cases = [
        // Indexed ILBM, HAM (which takes the fixed-RGBA path instead), a
        // per-scanline palette, a truecolour IFF, both DEGAS variants and
        // the ST's two other still-image formats.
        ("testdata/test.iff", "Amiga AGA 640x512 (256 colors)"),
        ("testdata/iffILBM/FearFace.HAM8", "Amiga AGA 640x512 (HAM8)"),
        ("testdata/iffILBM/sham.iff", "Amiga OCS 640x512 (HAM6/SHAM)"),
        (
            "testdata/iffILBM/Vogel_Kamera.24",
            "Amiga 148x262 (True color)",
        ),
        ("testdata/degas/FUSE.PI1", "Atari 320x200 (16 colors)"),
        ("testdata/degas/BOLEK3.PC1", "Atari 320x200 (16 colors)"),
        ("testdata/degas/ST4EVER.NEO", "Atari 320x200 (16 colors)"),
        ("testdata/degas/ATARIMAN.CA1", "Atari 320x200 (16 colors)"),
        ("testdata/degas/EXO7.KID", "Atari 448x274 (16 colors)"),
    ];
    for (file, expected) in cases {
        let emu = ImageEmu::new(&root(file)).unwrap();
        assert_eq!(emu.get_info().as_deref(), Some(expected), "{file}");
    }

    // A ZX screen has no header to describe, so it is always the same line.
    let mut bytes = vec![0b1111_0000u8; 6144];
    bytes.resize(6912, 0x80 | 0x40 | 0x06 | (1 << 3));
    let path = std::env::temp_dir().join("image_emu_info.scr");
    std::fs::write(&path, &bytes).unwrap();
    let emu = ImageEmu::new(&path).unwrap();
    assert_eq!(emu.get_info().as_deref(), Some("ZX Spectrum 256x192 (SCR)"));
    let _ = std::fs::remove_file(&path);

    // The `image` crate fallback names the format it sniffed, not the
    // extension — so a PNG saved under the wrong name is still a PNG. Every
    // pixel a different colour, which is past the count worth reporting, so
    // the depth is what describes it.
    let mut src = image::RgbImage::new(32, 16);
    for (x, y, px) in src.enumerate_pixels_mut() {
        *px = image::Rgb([x as u8, y as u8, 0]);
    }
    let path = std::env::temp_dir().join("image_emu_info.bmp");
    src.save_with_format(&path, image::ImageFormat::Png)
        .unwrap();
    let emu = ImageEmu::new(&path).unwrap();
    assert_eq!(emu.get_info().as_deref(), Some("PNG 32x16 (True color)"));
    let _ = std::fs::remove_file(&path);
}

/// A truecolour image that only uses a few colours is described by that
/// count rather than by its storage depth — the depth returns as soon as
/// there are more colours than are worth counting.
#[test]
fn get_info_counts_colors_of_truecolor_images() {
    let dir = std::env::temp_dir();
    // 24-bit with three colours, 32-bit with one, and one colour past the
    // limit: 257 distinct pixels in a 257x1 image.
    let mut three = image::RgbImage::new(30, 10);
    for (x, _, px) in three.enumerate_pixels_mut() {
        *px = image::Rgb([[10u8, 120, 250][x as usize % 3], 0, 0]);
    }
    let mut one = image::RgbaImage::new(8, 8);
    for px in one.pixels_mut() {
        *px = image::Rgba([1, 2, 3, 255]);
    }
    let mut many = image::RgbImage::new(257, 1);
    for (x, _, px) in many.enumerate_pixels_mut() {
        *px = image::Rgb([x as u8, (x >> 8) as u8, 0]);
    }

    for (name, expected) in [
        ("three", "PNG 30x10 (3 colors)"),
        ("one", "PNG 8x8 (1 color)"),
        ("many", "PNG 257x1 (True color)"),
    ] {
        let path = dir.join(format!("image_emu_count_{name}.png"));
        match name {
            "three" => three.save(&path).unwrap(),
            "one" => one.save(&path).unwrap(),
            _ => many.save(&path).unwrap(),
        }
        let emu = ImageEmu::new(&path).unwrap();
        assert_eq!(emu.get_info().as_deref(), Some(expected), "{name}");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn image_emu_presents_ilbm_frame() {
    let mut emu = ImageEmu::new(&root("testdata/test.iff")).unwrap();
    assert_eq!(emu.get_frame_size(), (640, 512));
    // `run` always succeeds; the frame is ready immediately.
    assert!(emu.run());
    emu.with_frame(&mut |w, h, frame| {
        assert_eq!((w, h), (640, 512));
        assert_eq!(frame.len(), w * h);
        // The decoded image must not be a single flat color.
        let first = frame[0];
        assert!(
            frame.iter().any(|&px| px != first),
            "presented frame is blank"
        );
    });
}

/// Atari ST pictures reach the same indexed path as ILBM: DEGAS plain
/// (`.PI1`) and compressed (`.PC1`), NEOchrome, CrackArt and the
/// overscanned KID alike.
#[test]
fn image_emu_presents_degas_frames() {
    for (file, size) in [
        ("testdata/degas/FUSE.PI1", (320, 200)),
        ("testdata/degas/BOLEK3.PC1", (320, 200)),
        ("testdata/degas/ST4EVER.NEO", (320, 200)),
        ("testdata/degas/ATARIMAN.CA1", (320, 200)),
        ("testdata/degas/EXO7.KID", (448, 274)),
    ] {
        let mut emu = ImageEmu::new(&root(file)).unwrap();
        assert_eq!(emu.get_frame_size(), size, "wrong size for {file}");
        assert!(emu.run());
        emu.with_frame(&mut |w, h, frame| {
            assert_eq!((w, h), size);
            let first = frame[0];
            assert!(
                frame.iter().any(|&px| px != first),
                "presented {file} frame is blank"
            );
        });
    }
}

/// A ZX Spectrum screen dump reaches the indexed path too, and its flashing
/// attributes arrive as cycling ranges that animate the presented frame.
#[test]
fn image_emu_presents_zx_screen() {
    // Half the pixels lit, every cell flashing bright yellow on blue.
    let mut bytes = vec![0b1111_0000u8; 6144];
    bytes.resize(6912, 0x80 | 0x40 | 0x06 | (1 << 3));
    let path = std::env::temp_dir().join("image_emu_test.scr");
    std::fs::write(&path, &bytes).unwrap();

    let mut emu = ImageEmu::new(&path).unwrap();
    assert_eq!(emu.get_frame_size(), (256, 192));
    assert_eq!(emu.ranges.len(), 1, "expected one FLASH range");
    // The two halves of a flash are well inside a second of each other, and
    // the first four pixels of a byte are ink where the next four are paper.
    emu.render(0.0);
    let before = emu.frame.clone();
    emu.render(0.4);
    assert_ne!(before[0], before[4], "ink and paper are the same colour");
    assert_eq!(before[0], emu.frame[4], "FLASH did not exchange the two");
    assert_eq!(before[4], emu.frame[0]);
    assert!(emu.run());
    let _ = std::fs::remove_file(&path);
}

/// A ZX screen is recognised by its size, so an ordinary image that happens
/// to be the same size must still decode as itself.
#[test]
fn zx_screen_sizes_do_not_capture_other_formats() {
    // 48x48 RGB is 6912 bytes of pixel data — the size of a `.SCR` — and
    // BMP stores it uncompressed, so the file is that plus its header.
    let mut src = image::RgbImage::new(48, 48);
    for (x, y, px) in src.enumerate_pixels_mut() {
        *px = image::Rgb([(x * 5) as u8, (y * 5) as u8, 96]);
    }
    for ext in ["bmp", "png"] {
        let path = std::env::temp_dir().join(format!("image_emu_zx_clash.{ext}"));
        src.save(&path).unwrap();
        let emu = ImageEmu::new(&path).unwrap();
        assert_eq!(
            emu.get_frame_size(),
            (48, 48),
            "{ext} decoded as something else"
        );
        assert!(emu.palette.is_empty(), "{ext} took the indexed path");
        let _ = std::fs::remove_file(&path);
    }
}

/// A minimal 8-bit paletted PCX, since the `image` crate can decode PCX
/// (through `image_extras`) but not write it. `width` x `height` pixels of
/// `index`, against a palette where that entry is the only non-black one.
fn write_pcx(path: &Path, width: u16, height: u16, index: u8) {
    let mut buf = vec![0u8; 128];
    buf[0] = 0x0a; // ZSoft manufacturer marker
    buf[1] = 5; // version: 3.0, i.e. with a 256-colour palette at the end
    buf[2] = 1; // RLE encoded, the only encoding PCX defines
    buf[3] = 8; // bits per pixel per plane
    // The window, as inclusive bounds: xmin, ymin, xmax, ymax. xmin/ymin
    // stay zero, so only the maxima need writing.
    buf[8..10].copy_from_slice(&(width - 1).to_le_bytes());
    buf[10..12].copy_from_slice(&(height - 1).to_le_bytes());
    buf[65] = 1; // one colour plane
    buf[66..68].copy_from_slice(&width.to_le_bytes()); // bytes per line (even)
    buf[68..70].copy_from_slice(&1u16.to_le_bytes()); // colour, not greyscale

    // Every pixel as its own one-byte run, which is valid RLE whatever the
    // index is — a bare literal would need escaping once it reaches 0xC0.
    for _ in 0..height {
        for _ in 0..width {
            buf.extend_from_slice(&[0xc1, index]);
        }
    }

    buf.push(0x0c); // marks the trailing 256-entry palette
    let mut palette = vec![0u8; 768];
    palette[index as usize * 3..index as usize * 3 + 3].copy_from_slice(&[200, 100, 50]);
    buf.extend_from_slice(&palette);
    std::fs::write(path, buf).unwrap();
}

/// PNG/BMP/JPEG/TGA still images decode through the same path as ILBM, via
/// the `image` crate fallback. Each is round-tripped through a temp file.
#[test]
fn image_emu_presents_still_formats() {
    // A small non-flat gradient so a mis-decode (blank frame) is caught.
    // RGB (not RGBA) so the same buffer can be saved as JPEG, which has no
    // alpha channel.
    let mut src = image::RgbImage::new(8, 6);
    for (x, y, px) in src.enumerate_pixels_mut() {
        *px = image::Rgb([(x * 32) as u8, (y * 40) as u8, 128]);
    }
    let dir = std::env::temp_dir();
    for ext in ["png", "bmp", "jpg", "tga"] {
        let path = dir.join(format!("image_emu_test.{ext}"));
        src.save(&path).unwrap();
        let mut emu = ImageEmu::new(&path).unwrap();
        assert_eq!(emu.get_frame_size(), (8, 6), "wrong size for .{ext}");
        assert!(emu.run());
        emu.with_frame(&mut |w, h, frame| {
            assert_eq!((w, h), (8, 6));
            assert_eq!(frame.len(), w * h);
            let first = frame[0];
            assert!(
                frame.iter().any(|&px| px != first),
                "presented .{ext} frame is blank"
            );
        });
        let _ = std::fs::remove_file(&path);
    }
}

/// A truecolour TGA opens with two zero bytes, which read as a valid DEGAS
/// low-resolution word — so any TGA holding at least a screenful of data
/// used to be decoded as an ST picture and presented as noise.
#[test]
fn tga_is_not_mistaken_for_a_degas_screen() {
    // Comfortably past the 34 + 32000 bytes the DEGAS decoder needs, so it
    // would find a whole screen to misread rather than bail out short.
    let mut src = image::RgbaImage::new(200, 100);
    for (x, y, px) in src.enumerate_pixels_mut() {
        *px = image::Rgba([x as u8, y as u8, 96, 255]);
    }
    let path = std::env::temp_dir().join("image_emu_tga_degas.tga");
    src.save(&path).unwrap();

    let emu = ImageEmu::new(&path).unwrap();
    assert_eq!(emu.get_frame_size(), (200, 100));
    assert!(emu.palette.is_empty(), "TGA took the indexed path");
    emu.with_frame(&mut |_, _, frame| {
        let expected: Vec<u32> = src.pixels().map(|px| u32::from_ne_bytes(px.0)).collect();
        assert_eq!(frame, expected, "TGA pixels did not survive decoding");
    });
    let _ = std::fs::remove_file(&path);
}

/// PCX reaches the same fallback, but only because `load_image` registers
/// the extra decoder with the `image` crate first.
#[test]
fn image_emu_presents_pcx() {
    let path = std::env::temp_dir().join("image_emu_test.pcx");
    write_pcx(&path, 8, 6, 7);
    let emu = ImageEmu::new(&path).unwrap();
    assert_eq!(emu.get_frame_size(), (8, 6));
    emu.with_frame(&mut |w, h, frame| {
        assert_eq!((w, h), (8, 6));
        // Flat by construction, so the palette lookup is checked exactly
        // rather than just for not being blank.
        let expected = u32::from_ne_bytes([200, 100, 50, 255]);
        assert!(
            frame.iter().all(|&px| px == expected),
            "PCX palette entry did not survive decoding"
        );
    });
    let _ = std::fs::remove_file(&path);
}

#[test]
fn color_cycling_changes_the_frame() {
    // Cycling is opt-in, so enable it via the tag `--color-cycle` sets.
    let mut emu = ImageEmu::new(&root("testdata/test.iff")).unwrap();
    // test.iff carries active CRNG chunks, so there is something to cycle.
    assert!(
        !emu.ranges.is_empty(),
        "expected active colour-cycling ranges"
    );

    // Render two well-separated points in the animation and confirm the
    // rotated palette actually changes the presented pixels.
    emu.render(0.0);
    let frame_a = emu.frame.clone();
    emu.render(1.0);
    let frame_b = emu.frame.clone();
    assert_ne!(frame_a, frame_b, "colour cycling did not change the frame");
}
