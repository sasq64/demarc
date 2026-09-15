use std::collections::HashMap;
use std::path::Path;

/// An 80x25 text mode is 640x200 pixels, but the card blits its overscan
/// border too, putting the first character cell 8 pixels in and 4 down.
const BORDER_X: usize = 8;
const BORDER_Y: usize = 4;
pub const COLS: usize = 80;
pub const ROWS: usize = 25;
const CELL: usize = 8;

/// Anything brighter than this in any channel counts as foreground. CGA
/// text uses a fixed 16-colour palette with nothing near the middle, so
/// there is no borderline case to get wrong.
const LIT: u8 = 96;

/// Maps an 8x8 glyph bitmap back to its character code.
pub type Font = HashMap<[u8; 8], u8>;

/// Load the 8x8 CGA font PCem renders text modes with.
///
/// `loadfont(.., FONT_MDA)` in PCem's video.c reads `mda.rom` as four
/// 2048-byte blocks — the two halves of the 8x14 MDA font, then the thin
/// and the thick 8x8 CGA fonts. The last block is the one CGA text uses.
pub fn load_font(roms: &Path) -> Font {
    let rom = std::fs::read(roms.join("mda.rom")).expect("mda.rom (the CGA font) is missing");
    assert!(rom.len() >= 8192, "mda.rom is too short to hold four fonts");

    let mut font = Font::new();
    for ch in 0..=255u8 {
        let off = 6144 + usize::from(ch) * 8;
        let glyph: [u8; 8] = rom[off..off + 8].try_into().unwrap();
        // Codes can share a bitmap — NUL and space are both blank — and
        // first one wins, so a blank cell reads back as NUL, not space.
        // decode() turns both into a space anyway.
        font.entry(glyph).or_insert(ch);
    }
    font
}

/// Decode a CGA text-mode frame into its 25 lines of text.
///
/// Cells are matched against the font both as-is and inverted, so the
/// black-on-white function key bar along the bottom of the BASIC screen
/// reads like everything else. A cell matching neither becomes `?`.
pub fn decode(width: usize, height: usize, pixels: &[u32], font: &Font) -> Vec<String> {
    assert!(
        width >= BORDER_X + COLS * CELL && height >= BORDER_Y + ROWS * CELL,
        "frame is {width}x{height}, too small for an 80x25 text mode"
    );

    (0..ROWS)
        .map(|row| {
            let mut line = String::with_capacity(COLS);
            for col in 0..COLS {
                let mut glyph = [0u8; 8];
                for (y, bits) in glyph.iter_mut().enumerate() {
                    for x in 0..CELL {
                        let i = (BORDER_Y + row * CELL + y) * width + BORDER_X + col * CELL + x;
                        let [r, g, b, _] = pixels[i].to_ne_bytes();
                        if r > LIT || g > LIT || b > LIT {
                            *bits |= 0x80 >> x;
                        }
                    }
                }
                let inverse = glyph.map(|b| !b);
                match font.get(&glyph).or_else(|| font.get(&inverse)) {
                    Some(&ch) if (0x20..0x7f).contains(&ch) => line.push(ch as char),
                    // Control codes and the line-drawing half of the set:
                    // present, but not what this test reads.
                    Some(_) => line.push(' '),
                    None => line.push('?'),
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}
