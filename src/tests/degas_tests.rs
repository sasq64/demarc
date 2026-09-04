use std::path::PathBuf;

use super::*;

/// Paths here are rooted at the crate directory rather than left relative:
/// a conversion running in another test switches the process-wide working
/// directory for its duration (see `cbmconvert::CwdGuard`).
/// One sample per format, with the size it decodes to.
const SAMPLES: [(&str, (u32, u32)); 5] = [
    ("FUSE.PI1", (320, 200)),
    ("BOLEK3.PC1", (320, 200)),
    ("ST4EVER.NEO", (320, 200)),
    ("ATARIMAN.CA1", (320, 200)),
    ("EXO7.KID", (448, 274)),
];

fn get_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata/degas")
        .join(name)
}

/// Every format's sample file, low-res but for KID's opened borders, each
/// decoding to a screen that uses most of its palette — a plane mix-up
/// would still fill the frame, but a layout mix-up between the two DEGAS
/// variants would not agree on the picture, which the next test checks. For
/// the packed formats it is also the unpacking that is on trial: a run
/// length off by one desynchronises the stream, and what comes out is noise
/// rather than a picture.
#[test]
fn decodes_every_format() {
    for (file, (width, height)) in SAMPLES {
        let img = load_indexed(get_path(file)).unwrap();
        assert_eq!(
            (img.width, img.height),
            (width, height),
            "wrong size for {file}"
        );
        assert_eq!(img.palette.len(), 16);
        assert_eq!(img.indices.len() as u32, width * height);
        let first = img.indices[0];
        assert!(
            img.indices.iter().any(|&i| i != first),
            "{file} decoded to a single flat colour"
        );
        // A real picture reaches well past the first few registers; a
        // misread plane layout tends to collapse into a handful.
        let used = (0..16u8).filter(|c| img.indices.contains(c)).count();
        assert!(used > 8, "{file} only uses {used} of 16 colours");
    }
}

/// The compressed layout is not just the uncompressed one packed: planes
/// are stored a whole scanline at a time. Re-packing the interleaved bytes
/// of a `.PI1` and reading them back the compressed way must therefore
/// produce the same picture only when the layouts are handled separately.
#[test]
fn layouts_agree_after_reordering() {
    let bytes = fs::read(get_path("FUSE.PI1")).unwrap();
    let mode = mode_for(0).unwrap();
    let screen = &bytes[HEADER_BYTES..HEADER_BYTES + SCREEN_BYTES];

    // Interleaved -> sequential: gather each plane's words for a scanline.
    let mut sequential = Vec::with_capacity(SCREEN_BYTES);
    for y in 0..mode.height {
        let row = y * 160;
        for p in 0..mode.planes {
            for w in 0..20 {
                let at = row + w * mode.planes * 2 + p * 2;
                sequential.extend_from_slice(&screen[at..at + 2]);
            }
        }
    }

    assert_eq!(
        decode_indices(screen, mode, Layout::Interleaved),
        decode_indices(&sequential, mode, Layout::Sequential)
    );
}

/// ST palettes use three bits per component and STE ones four, with the
/// extra bit at the bottom. The width is inferred from the colours present.
#[test]
fn palette_widths() {
    // No component sets bit 3, so this is a plain ST palette: 7 is white.
    let mut header = vec![0u8; HEADER_BYTES];
    header[2..4].copy_from_slice(&0x0777u16.to_be_bytes());
    header[4..6].copy_from_slice(&0x0700u16.to_be_bytes());
    let pal = parse_palette(&header, 2, 16);
    assert_eq!(pal[0], [255, 255, 255]);
    assert_eq!(pal[1], [255, 0, 0]);
    assert_eq!(pal[2], [0, 0, 0]);

    // One entry using bit 3 switches the whole palette to STE, where 0xf
    // is white and 0x7 is the value just below middle.
    header[6..8].copy_from_slice(&0x0fffu16.to_be_bytes());
    let pal = parse_palette(&header, 2, 16);
    assert_eq!(pal[2], [255, 255, 255]);
    assert_eq!(pal[0], [238, 238, 238]);
}

/// A DEGAS Elite colour-animation trailer becomes cycle ranges; unused
/// channels (all zeroes) and switched-off ones are dropped.
#[test]
fn elite_colour_animation() {
    let mut bytes = fs::read(get_path("FUSE.PI1")).unwrap();
    assert!(
        load_indexed_from_memory(&bytes).unwrap().ranges.is_empty(),
        "a plain DEGAS file has no animation"
    );

    let mut trailer = [0u8; TRAILER_BYTES];
    let mut put = |word: usize, value: u16| {
        trailer[word * 2..word * 2 + 2].copy_from_slice(&value.to_be_bytes());
    };
    // Channel 0: registers 2..=5, rightwards, one step every 8 vblanks.
    put(0, 2);
    put(ANIM_CHANNELS, 5);
    put(ANIM_CHANNELS * 2, 2);
    put(ANIM_CHANNELS * 3, (MAX_ANIM_DELAY - 8) as u16);
    // Channel 1: a valid range that is switched off.
    put(1, 8);
    put(ANIM_CHANNELS + 1, 12);
    put(ANIM_CHANNELS * 2 + 1, 1);
    bytes.extend_from_slice(&trailer);

    let ranges = load_indexed_from_memory(&bytes).unwrap().ranges;
    assert_eq!(ranges.len(), 1);
    assert_eq!((ranges[0].low, ranges[0].high), (2, 5));
    assert!(ranges[0].active && !ranges[0].reverse);
    assert_eq!(ranges[0].rate, (CRNG_RATE_60HZ / 8) as u16);
}

/// NEOchrome's single animation channel. The sample file has a range set up
/// but switched off, which is what most `.NEO` files in the wild hold, and
/// the two switched-on cases are made from it.
#[test]
fn neo_colour_animation() {
    let mut bytes = fs::read(get_path("ST4EVER.NEO")).unwrap();
    let ranges = load_indexed_from_memory(&bytes).unwrap().ranges;
    assert_eq!(ranges.len(), 1);
    assert_eq!((ranges[0].low, ranges[0].high), (1, 15));
    assert!(!ranges[0].active, "the sample's animation is switched off");

    // Switched on, three vblanks per step (the byte counts one more than
    // it waits), cycling rightwards.
    bytes[NEO_ANIM_SPEED..NEO_ANIM_SPEED + 2].copy_from_slice(&0x8004u16.to_be_bytes());
    let ranges = load_indexed_from_memory(&bytes).unwrap().ranges;
    assert!(ranges[0].active && !ranges[0].reverse);
    assert_eq!(ranges[0].rate, (CRNG_RATE_60HZ / 3) as u16);

    // A negative count cycles the other way, and one step per vblank is as
    // fast as it goes.
    bytes[NEO_ANIM_SPEED..NEO_ANIM_SPEED + 2].copy_from_slice(&0x80ffu16.to_be_bytes());
    let ranges = load_indexed_from_memory(&bytes).unwrap().ranges;
    assert!(ranges[0].active && ranges[0].reverse);
    assert_eq!(ranges[0].rate, CRNG_RATE_60HZ as u16);

    // Without the top bit of the limits word there is nothing set up at
    // all, however tempting the nibbles look.
    bytes[NEO_ANIM_LIMITS] = 0;
    assert!(load_indexed_from_memory(&bytes).unwrap().ranges.is_empty());
}

/// CrackArt's five commands, each writing where the offset says. With an
/// offset of one, that is simply front to back.
#[test]
fn crackart_commands() {
    let mut stream = vec![0xff, 0x77, 0x00, 0x01];
    stream.extend_from_slice(&[0x11]); // a literal
    stream.extend_from_slice(&[0xff, 0, 1, 0x22]); // a byte-counted run
    stream.extend_from_slice(&[0xff, 1, 0, 2, 0x33]); // a word-counted one
    stream.extend_from_slice(&[0xff, 3, 0x44]); // the short form, four of them
    stream.extend_from_slice(&[0xff, 0xff]); // the escape byte itself
    stream.extend_from_slice(&[0xff, 2, 1, 0]); // step over 257 fill bytes
    stream.extend_from_slice(&[0x55]);
    stream.extend_from_slice(&[0xff, 2, 0]); // end of picture

    let out = unpack_crackart(&stream).unwrap();
    let mut want = vec![0x11];
    want.extend_from_slice(&[0x22; 2]);
    want.extend_from_slice(&[0x33; 3]);
    want.extend_from_slice(&[0x44; 4]);
    want.push(0xff);
    want.extend_from_slice(&[0x77; 257]);
    want.push(0x55);
    assert_eq!(out[..want.len()], want);
    // Everything the stream never reached keeps the fill byte.
    assert!(out[want.len()..].iter().all(|&b| b == 0x77));
}

/// The offset is what makes the format pack as well as it does: a scanline
/// of it walks down a column of the screen, and past the bottom it comes
/// back to the top one byte further along.
#[test]
fn crackart_wraps_into_the_next_column() {
    let mut stream = vec![0xff, 0x00, 0x00, 160];
    // One byte per scanline down the first column, then one more.
    stream.extend(std::iter::repeat_n(0x01, 200));
    stream.push(0x02);
    let out = unpack_crackart(&stream).unwrap();

    assert!((0..200).all(|y| out[y * 160] == 0x01));
    assert_eq!(out[1], 0x02, "the 201st byte starts the next column");

    // A stream with nothing in it is a screen of nothing but the fill
    // byte, whatever the offset says — including an offset with its top
    // bit set, which is not part of the offset at all.
    for offset in [[0, 0], [0xff, 0xff]] {
        let flat = unpack_crackart(&[&[0xff, 0x42][..], &offset].concat()).unwrap();
        assert_eq!(flat, vec![0x42; SCREEN_BYTES]);
    }
}

/// An uncompressed CrackArt file is the same screen memory a `.PI1` holds,
/// behind a four-byte header and a palette of only the registers the mode
/// uses. Built from the sample so the two must decode to the same picture.
#[test]
fn crackart_uncompressed() {
    let degas = fs::read(get_path("FUSE.PI1")).unwrap();
    let mut bytes = vec![b'C', b'A', 0, 0];
    bytes.extend_from_slice(&degas[2..HEADER_BYTES]);
    bytes.extend_from_slice(&degas[HEADER_BYTES..HEADER_BYTES + SCREEN_BYTES]);

    let ca = load_indexed_from_memory(&bytes).unwrap();
    let pi1 = load_indexed_from_memory(&degas).unwrap();
    assert_eq!(ca.indices, pi1.indices);
    assert_eq!(ca.palette, pi1.palette);
    assert!(is_st_image(&bytes[..SNIFF_BYTES], bytes.len()));
}

/// A KID file is an overscanned low-resolution screen behind DEGAS'
/// palette: 274 scanlines of 230 bytes, of which the last six hold no
/// pixel this decodes. Built here from the `.PI1` sample, whose 160-byte
/// lines go in at the left, so the picture must come back out in the same
/// place — a stride read as the width alone would shear it across the
/// screen, which a real KID (checked in the test above) still fills.
#[test]
fn kid_overscan() {
    let degas = fs::read(get_path("FUSE.PI1")).unwrap();
    let mut bytes = KID_MAGIC.to_vec();
    bytes.extend_from_slice(&degas[2..HEADER_BYTES]);
    for y in 0..KID_HEIGHT {
        let mut line = vec![0u8; KID_STRIDE];
        let at = HEADER_BYTES + y * 160;
        if let Some(row) = degas.get(at..at + 160) {
            line[..160].copy_from_slice(row);
        }
        bytes.extend_from_slice(&line);
    }
    assert_eq!(bytes.len(), KID_BYTES);

    let kid = load_indexed_from_memory(&bytes).unwrap();
    let pi1 = load_indexed_from_memory(&degas).unwrap();
    assert_eq!((kid.width, kid.height), (448, 274));
    assert_eq!(kid.palette, pi1.palette);
    for y in 0..200 {
        assert_eq!(
            kid.indices[y * 448..y * 448 + 320],
            pi1.indices[y * 320..(y + 1) * 320],
            "row {y}"
        );
    }
    assert!(is_st_image(&bytes[..SNIFF_BYTES], bytes.len()));
    assert_eq!(describe(&bytes), "Atari 448x274 (16 colors)");
    // There is one size a KID file can have, and this is not it.
    assert!(!is_st_image(&bytes[..SNIFF_BYTES], bytes.len() - 1));
    bytes.truncate(bytes.len() - 1);
    assert!(load_indexed_from_memory(&bytes).is_err());
}

#[test]
fn sniffing() {
    for (file, _) in SAMPLES {
        let bytes = fs::read(get_path(file)).unwrap();
        assert!(
            is_st_image(&bytes[..SNIFF_BYTES], bytes.len()),
            "{file} not sniffed"
        );
    }
    // Right shape, wrong size for an uncompressed screen.
    let bytes = fs::read(get_path("FUSE.PI1")).unwrap();
    assert!(!is_st_image(&bytes[..SNIFF_BYTES], bytes.len() - 1));
    // A palette word with a set top nibble is not an ST colour.
    let mut bad = bytes.clone();
    bad[2] = 0x10;
    assert!(!is_st_image(&bad[..SNIFF_BYTES], bad.len()));
    // A NEOchrome header is nearly all zeroes, so its exact size is most of
    // what tells it apart.
    let neo = fs::read(get_path("ST4EVER.NEO")).unwrap();
    assert!(!is_st_image(&neo[..SNIFF_BYTES], neo.len() + 1));
    // Resolution 3 does not exist, in any of the three.
    assert!(!is_st_image(&[0, 3], 32034));
    assert!(load_indexed_from_memory(&[0, 3]).is_err());
    assert!(load_indexed_from_memory(b"CA\x01\x03").is_err());
}

/// Every format is described by what it holds rather than by its name, and
/// a file that says nothing useful is still described.
#[test]
fn describes_each_format() {
    for (file, (width, height)) in SAMPLES {
        let bytes = fs::read(get_path(file)).unwrap();
        assert_eq!(
            describe(&bytes),
            format!("Atari {width}x{height} (16 colors)"),
            "{file}"
        );
    }
    // Medium and high resolution, in the two headers that can say so
    // without a whole screen behind them.
    assert_eq!(describe(b"CA\x01\x01"), "Atari 640x400 (4 colors)");
    assert_eq!(describe(b"CA\x01\x02"), "Atari 640x400 (2 colors)");
    assert_eq!(describe(&[0x80, 0x01]), "Atari 640x400 (4 colors)");
    assert_eq!(describe(b"CA\x01\x09"), "Atari");
    assert_eq!(describe(&[]), "Atari");
}
