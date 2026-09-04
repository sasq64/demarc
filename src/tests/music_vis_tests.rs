use super::*;

/// Write `source` to a uniquely named file so the tests can run in
/// parallel, and hand back a visualizer for it.
fn vis(name: &str, source: &str) -> Result<(Visualizer, PathBuf)> {
    vis_sized(name, source, 4, 2)
}

fn vis_sized(
    name: &str,
    source: &str,
    width: usize,
    height: usize,
) -> Result<(Visualizer, PathBuf)> {
    let path = std::env::temp_dir().join(format!("music_vis_{name}.lua"));
    std::fs::write(&path, source).unwrap();
    Visualizer::new(&path, width, height).map(|v| (v, path))
}

/// The whole round trip in one assertion: a colour built by `rgb()`, laid
/// down by Luau's little-endian `buffer.writeu32`, read back as a
/// native-order `u32`, must equal what the backend's own `rgb` produces.
/// Get the byte order wrong and red and blue swap.
#[test]
fn a_pixel_survives_the_round_trip() {
    let (x, y) = (2usize, 1usize);
    let (mut v, path) = vis(
        "pixel",
        r#"
        function Render(buf)
            buffer.writeu32(buf, (1 * WIDTH + 2) * 4, rgb(0x11, 0x22, 0x33))
        end
        "#,
    )
    .unwrap();

    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();

    let expected = u32::from_ne_bytes([0x11, 0x22, 0x33, 0xff]);
    assert_eq!(frame[y * 4 + x], expected, "wrong colour or wrong position");
    assert!(
        frame.iter().filter(|&&px| px == expected).count() == 1,
        "the pixel landed more than once"
    );
    let _ = std::fs::remove_file(&path);
}

/// `rgb`'s alpha defaults to opaque, and the drawing helpers agree with
/// `writeu32` about the packing.
#[test]
fn the_helpers_and_writeu32_agree() {
    let (mut v, path) = vis(
        "helpers",
        r#"
        function Render(buf)
            clear(buf, rgb(1, 2, 3))
            box(buf, 0, 0, WIDTH, 1, rgb(4, 5, 6, 7))
        end
        "#,
    )
    .unwrap();

    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();

    let cleared = u32::from_ne_bytes([1, 2, 3, 255]);
    let line = u32::from_ne_bytes([4, 5, 6, 7]);
    assert!(frame[..4].iter().all(|&px| px == line), "box: {frame:?}");
    assert!(
        frame[4..].iter().all(|&px| px == cleared),
        "clear: {frame:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// Rectangles that run off the edge are clipped, not rejected: a script
/// deriving coordinates from a waveform will overshoot now and then, and
/// blanking the whole frame over it would be a poor trade.
#[test]
fn out_of_range_boxes_are_clipped() {
    let (mut v, path) = vis(
        "clip",
        r#"
        function Render(buf)
            clear(buf, rgb(0, 0, 0))
            box(buf, -100, 0, 200, 1, rgb(9, 9, 9))     -- straddles the left edge
            box(buf, 2, -5, 1, 500, rgb(8, 8, 8))       -- taller than the frame
            box(buf, 0, 999, 2, 2, rgb(7, 7, 7))        -- entirely off-screen
            box(buf, 0, 0, -4, -4, rgb(6, 6, 6))        -- negative extents
        end
        "#,
    )
    .unwrap();

    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).expect("clipping must not error");
    assert_eq!(
        frame[3],
        u32::from_ne_bytes([9, 9, 9, 255]),
        "clipped to the right edge"
    );
    assert_eq!(
        frame[4 + 2],
        u32::from_ne_bytes([8, 8, 8, 255]),
        "clipped to the bottom"
    );
    for absent in [[7, 7, 7, 255], [6, 6, 6, 255]] {
        assert!(
            !frame.iter().any(|&px| px == u32::from_ne_bytes(absent)),
            "an empty rectangle drew something: {absent:?}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// A script that does not compile fails at load, rather than panicking or
/// producing a `Visualizer` that throws on every frame.
#[test]
fn a_syntax_error_fails_to_load() {
    let Err(err) = vis("syntax", "function Render(buf) this is not lua") else {
        panic!("a script that does not compile must not load");
    };
    assert!(
        format!("{err:#}").contains("music_vis_syntax.lua"),
        "the error should name the script: {err:#}"
    );
}

/// So does one that never defines `render` — better caught at load than as
/// a blank window.
#[test]
fn a_missing_render_fails_to_load() {
    let Err(err) = vis("norender", "function nope() end") else {
        panic!("a script without render() must not load");
    };
    assert!(
        format!("{err:#}").contains("Render"),
        "the error should mention render: {err:#}"
    );
}

/// A script that throws mid-frame is a per-call error, not a load error:
/// the caller decides what to show, and can keep asking.
#[test]
fn a_throwing_render_errors_per_call() {
    let (mut v, path) = vis("throw", "function Render(buf) error('boom') end").unwrap();
    let mut frame = vec![0u32; 4 * 2];
    assert!(v.render(&mut frame).is_err());
    assert!(
        v.render(&mut frame).is_err(),
        "the second call must also fail"
    );
    let _ = std::fs::remove_file(&path);
}

/// `init` runs once before the first frame, and what it leaves in a global
/// is still there when `render` looks.
#[test]
fn init_runs_before_the_first_frame() {
    let (mut v, path) = vis(
        "Init",
        r#"
        function Init() COLOUR = rgb(3, 4, 5) end
        function Render(buf) clear(buf, COLOUR) end
        "#,
    )
    .unwrap();
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    assert!(
        frame
            .iter()
            .all(|&px| px == u32::from_ne_bytes([3, 4, 5, 255]))
    );
    let _ = std::fs::remove_file(&path);
}

/// The script sees the samples the backend put there, at the scale the
/// documentation promises.
#[test]
fn get_samples_reaches_the_script() {
    let (mut v, path) = vis(
        "samples",
        r#"
        function Render(buf)
            local s = get_samples()
            clear(buf, rgb(#s, math.floor(s[1] * 100), math.floor(s[2] * 100)))
        end
        "#,
    )
    .unwrap();

    v.data().samples = vec![0.5, -0.25];
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    // -0.25 * 100 floors to -25, which wraps to 231 as a byte.
    assert_eq!(frame[0], u32::from_ne_bytes([2, 50, 231, 255]));
    let _ = std::fs::remove_file(&path);
}

/// Metadata the backend snapshotted reaches the script as a table, keyed by
/// name, with the sample rate alongside it.
#[test]
fn get_meta_reaches_the_script() {
    let (mut v, path) = vis(
        "meta",
        r#"
        function Render(buf)
            local m = get_meta()
            assert(m.composer == "Rob Hubbard", "composer was " .. tostring(m.composer))
            assert(m.sample_rate == 44100, "rate was " .. tostring(m.sample_rate))
            assert(m.nope == nil, "invented a key")
            clear(buf, rgb(#m.title, string.byte(m.title, 1), 0))
        end
        "#,
    )
    .unwrap();

    {
        let mut data = v.data();
        data.sample_rate = 44100.0;
        data.meta = vec![
            ("title".into(), b"Commando".to_vec()),
            ("composer".into(), b"Rob Hubbard".to_vec()),
        ];
    }
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    // "Commando" is 8 characters and starts with 'C' (67).
    assert_eq!(frame[0], u32::from_ne_bytes([8, 67, 0, 255]));
    let _ = std::fs::remove_file(&path);
}

/// A full-scale tone shows up in the spectrum, and only near its own
/// frequency. Also pins the per-frame caching: two calls, one transform.
#[test]
fn get_spectrum_finds_a_tone() {
    let (mut v, path) = vis(
        "spectrum",
        r#"
        function Render(buf)
            local a = get_spectrum(16)
            local b = get_spectrum(16)
            local peak, at = 0, 0
            for i = 1, #a do
                assert(a[i] == b[i], "cached spectrum differs")
                if a[i] > peak then peak, at = a[i], i end
            end
            clear(buf, rgb(at, math.floor(peak * 100), #a))
        end
        "#,
    )
    .unwrap();

    // A tone at one eighth of the sample rate, as interleaved stereo.
    {
        let mut data = v.data();
        data.sample_rate = 44100.0;
        data.samples = (0..FFT_SIZE)
            .flat_map(|i| {
                let s = (std::f32::consts::TAU * i as f32 / 8.0).sin();
                [s, s]
            })
            .collect();
    }
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();

    let [bin, peak, bins, _] = frame[0].to_ne_bytes();
    assert_eq!(bins, 16, "wrong number of bins");
    // Hann-corrected, a full-scale sine should come back near 1.0.
    assert!((80..=120).contains(&(peak as i32)), "peak was {peak}/100");
    // Sample rate / 8 is the top eighth of the spectrum, so the last few
    // log-spaced buckets.
    assert!(bin >= 13, "the tone landed in bucket {bin} of 16");
    let _ = std::fs::remove_file(&path);
}

/// Editing the script on disk replaces the running one, without the caller
/// doing anything but rendering the next frame.
#[test]
fn saving_the_script_reloads_it() {
    let (mut v, path) = vis(
        "reload",
        "function Render(buf) clear(buf, rgb(1, 1, 1)) end",
    )
    .unwrap();
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    assert_eq!(frame[0], u32::from_ne_bytes([1, 1, 1, 255]));

    // Saved the way editors actually save: write a temporary file and
    // rename it over the target. That replaces the inode, which is exactly
    // what a watch on the file itself would fail to follow -- so this is
    // the case the directory watch exists for.
    let tmp = path.with_extension("lua.tmp");
    std::fs::write(&tmp, "function Render(buf) clear(buf, rgb(2, 2, 2)) end").unwrap();
    std::fs::rename(&tmp, &path).unwrap();
    // The watcher is a background thread; give it a moment to notice rather
    // than assuming the write and the notification are ordered.
    let reloaded = (0..100).any(|_| {
        std::thread::sleep(std::time::Duration::from_millis(20));
        v.render(&mut frame).unwrap();
        frame[0] == u32::from_ne_bytes([2, 2, 2, 255])
    });
    assert!(reloaded, "the script was never reloaded");
    let _ = std::fs::remove_file(&path);
}

/// A save that leaves the file broken keeps the last working script, rather
/// than blanking the window over what is probably a half-typed edit.
#[test]
fn a_broken_reload_keeps_the_old_script() {
    let (mut v, path) = vis(
        "badreload",
        "function Render(buf) clear(buf, rgb(1, 1, 1)) end",
    )
    .unwrap();
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();

    std::fs::write(&path, "function Render(buf) this is not lua").unwrap();
    for _ in 0..25 {
        std::thread::sleep(std::time::Duration::from_millis(20));
        v.render(&mut frame)
            .expect("a broken reload must not fail the frame");
        assert_eq!(
            frame[0],
            u32::from_ne_bytes([1, 1, 1, 255]),
            "the broken script took over"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// A glyph lands where it was asked to, the right way round: the leftmost
/// pixel is the *most* significant bit of the row byte, and getting that
/// backwards mirrors every letter. Topaz's `A` starts `..##....`-doubled,
/// so row 0 is 0x18 -- pixels 3 and 4 and nothing else.
#[test]
fn text_draws_a_glyph() {
    let (mut v, path) = vis_sized(
        "text",
        r#"
        function Init() FONT = load_font("topaz") end
        function Render(buf)
            clear(buf, rgb(0, 0, 0))
            text(buf, FONT, 0, 0, "A", rgb(255, 255, 255))
            assert(FONT.width == 8 and FONT.height == 16, "wrong metrics")
        end
        "#,
        16,
        16,
    )
    .unwrap();

    let mut frame = vec![0u32; 16 * 16];
    v.render(&mut frame).unwrap();

    let ink = u32::from_ne_bytes([255, 255, 255, 255]);
    // Every row of the glyph cell, read back as the byte the font holds.
    let drawn: Vec<u8> = (0..16)
        .map(|y| {
            (0..8).fold(0u8, |bits, x| {
                bits | (((frame[y * 16 + x] == ink) as u8) << (7 - x))
            })
        })
        .collect();
    let font = Font::new(FONTS[0].1).unwrap();
    let expected: Vec<u8> = (0..16).map(|row| font.row(b'A', row)).collect();
    assert_eq!(drawn, expected, "the glyph came out wrong");
    // Nothing outside the 8-pixel cell, so `text` is not painting a box.
    assert!(
        (0..16).all(|y| (8..16).all(|x| frame[y * 16 + x] != ink)),
        "text drew past the glyph cell"
    );
    let _ = std::fs::remove_file(&path);
}

/// Characters advance by the glyph width, and text that runs off any edge is
/// clipped rather than erroring -- a song title is as long as it is, and the
/// script cannot know before it asks.
#[test]
fn text_advances_and_clips() {
    let (mut v, path) = vis_sized(
        "textclip",
        r#"
        function Init() FONT = load_font("topaz") end
        function Render(buf)
            clear(buf, rgb(0, 0, 0))
            text(buf, FONT, 0, 0, " A", rgb(255, 255, 255))  -- second cell
            text(buf, FONT, -8, 0, "A!", rgb(1, 2, 3))       -- first cell off left
            text(buf, FONT, 28, 0, "AA", rgb(4, 5, 6))       -- runs off the right
            text(buf, FONT, 0, -20, "A", rgb(7, 8, 9))       -- above the frame
            text(buf, FONT, 0, 100, "A", rgb(9, 8, 7))       -- below it
        end
        "#,
        32,
        16,
    )
    .expect("text off the edge must not fail to load");

    let mut frame = vec![0u32; 32 * 16];
    v.render(&mut frame).expect("clipping must not error");

    let font = Font::new(FONTS[0].1).unwrap();
    let ink = u32::from_ne_bytes([255, 255, 255, 255]);
    // ' ' is blank, so the 'A' sits in the second cell: x 8..16.
    for row in 0..16 {
        for col in 0..8 {
            let set = font.row(b'A', row) & (0x80 >> col) != 0;
            assert_eq!(
                frame[row * 32 + 8 + col] == ink,
                set,
                "advance is wrong at row {row}, column {col}"
            );
        }
    }
    // '!' is the second character of the string starting at -8, so it lands
    // in the first cell; the 'A' before it is entirely off-screen.
    let bang = u32::from_ne_bytes([1, 2, 3, 255]);
    assert!(
        (0..16).any(|row| (0..8).any(|col| frame[row * 32 + col] == bang)),
        "the character straddling the left edge vanished"
    );
    // Half a glyph over the right edge draws its left half and stops.
    let right = u32::from_ne_bytes([4, 5, 6, 255]);
    assert!(
        (0..16).any(|row| (28..32).any(|col| frame[row * 32 + col] == right)),
        "the character straddling the right edge vanished"
    );
    for absent in [[7, 8, 9, 255], [9, 8, 7, 255]] {
        assert!(
            !frame.iter().any(|&px| px == u32::from_ne_bytes(absent)),
            "text off the top or bottom drew something: {absent:?}"
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// Asking for a font that is not there is an error the script can see, and
/// it names the ones that are -- rather than handing back something that
/// draws blanks forever.
#[test]
fn an_unknown_font_errors() {
    let (mut v, path) = vis(
        "badfont",
        r#"
        function Render(buf)
            local ok, err = pcall(load_font, "helvetica")
            -- tostring: what pcall catches here is the host's error object,
            -- not a plain string.
            err = tostring(err)
            assert(not ok, "an unknown font loaded")
            assert(string.find(err, "topaz") ~= nil, "the error should list the fonts: " .. err)
            clear(buf, rgb(1, 2, 3))
        end
        "#,
    )
    .unwrap();
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    assert_eq!(frame[0], u32::from_ne_bytes([1, 2, 3, 255]));
    let _ = std::fs::remove_file(&path);
}

/// `noise()` stays in range, hands back something different every call,
/// and -- given arguments -- the *same* thing every time, which is what a
/// script placing stars relies on.
#[test]
fn noise_is_in_range_and_hashes_its_arguments() {
    let (mut v, path) = vis(
        "noise",
        r#"
        function Render(buf)
            local lo, hi, distinct = 1, 0, {}
            for i = 1, 1000 do
                local n = noise()
                assert(n >= 0 and n < 1, "out of range: " .. tostring(n))
                lo, hi = math.min(lo, n), math.max(hi, n)
                distinct[n] = true
            end
            local count = 0
            for _ in pairs(distinct) do count = count + 1 end
            assert(count > 990, "the stream repeats itself: " .. count)
            assert(lo < 0.05 and hi > 0.95, "not spread over 0..1")

            assert(noise(1, 2) == noise(1, 2), "hashed noise is not stable")
            assert(noise(1, 2) ~= noise(2, 1), "argument order is ignored")
            assert(noise(3) ~= noise(4), "neighbours hash the same")
            assert(noise(0) == noise(-0), "-0 and 0 hash differently")
            clear(buf, rgb(math.floor(noise(7) * 255), 0, 0))
        end
        "#,
    )
    .unwrap();
    let mut frame = vec![0u32; 4 * 2];
    v.render(&mut frame).unwrap();
    // Frame to frame the hashed value is the same, so the colour is too.
    let first = frame[0];
    v.render(&mut frame).unwrap();
    assert_eq!(frame[0], first, "noise(7) changed between frames");
    let _ = std::fs::remove_file(&path);
}

/// Every embedded font is the shape the drawing code assumes.
#[test]
fn the_embedded_fonts_decode() {
    for (name, glyphs) in FONTS {
        let font = Font::new(glyphs).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(font.height, 16, "{name} is not 8x16");
        assert_eq!(font.row(b' ', 0), 0, "{name}: space is not blank");
        assert!(
            (0..font.height).any(|row| font.row(b'A', row) != 0),
            "{name}: 'A' is blank"
        );
    }
}
