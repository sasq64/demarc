//! ZX Spectrum screen dumps (`.SCR`).
//!
//! The Spectrum has no picture format of its own: a `.SCR` is a byte-for-byte
//! copy of the 6912 bytes of video RAM, which is why the format has no header,
//! no palette and no version — the only thing that identifies it is its size.
//!
//! Video RAM is in two halves. First 6144 bytes of bitmap, one bit per pixel,
//! whose rows are stored in the order the ULA fetches them rather than top to
//! bottom (see [`row_offset`]). Then 768 attribute bytes, one per 8x8 character
//! cell, each naming the two colours that cell's set and clear bits take.
//!
//! Colour therefore has a resolution of its own, coarser than the bitmap's, and
//! the palette is fixed: eight colours at two brightness levels. Attributes also
//! carry a FLASH bit, which the ROM's interrupt routine honours by exchanging a
//! cell's two colours twice a second. That is a palette rotation over two
//! registers, so it is expressed as a [`CycleRange`] and animated by
//! [`ImageEmu`](crate::image_emu::ImageEmu) with the same code that runs an
//! ILBM's CRNG cycling or a DEGAS Elite colour animation (see [`crate::degas`]).

use std::{fs, path::Path};

use anyhow::{Result, ensure};

use crate::ilbm::{CycleRange, IndexedImage};

const WIDTH: usize = 256;
const HEIGHT: usize = 192;

/// The bitmap half: one bit per pixel, 32 bytes to a row.
const BITMAP_BYTES: usize = WIDTH / 8 * HEIGHT;

const ATTR_COLUMNS: usize = WIDTH / 8;
const ATTR_ROWS: usize = HEIGHT / 8;
/// The attribute half: one byte per character cell.
const ATTR_BYTES: usize = ATTR_COLUMNS * ATTR_ROWS;

/// A whole screen: bitmap plus attributes, and the size of a `.SCR`.
const FILE_BYTES: usize = BITMAP_BYTES + ATTR_BYTES;

/// Attribute bits: three of ink, three of paper, then BRIGHT and FLASH.
const INK: u8 = 0x07;
const PAPER: u8 = 0x38;
const BRIGHT: u8 = 0x40;
const FLASH: u8 = 0x80;

/// What the ROM leaves attributes at after a `CLS`, and so what an
/// attribute-less dump is shown with: white ink on black paper.
const CLS_ATTR: u8 = 0x07;

/// The fixed palette: eight colours, each at two brightness levels.
const PALETTE_COLOURS: usize = 16;

/// A non-BRIGHT component stops a little short of full — the ULA drives the
/// same pin, just without the brightness line pulling it all the way up.
const NORMAL_LEVEL: u8 = 0xd7;
const BRIGHT_LEVEL: u8 = 0xff;

/// Distinct flashing colour combinations an attribute byte can express: two
/// brightnesses times the ink/paper pairs whose colours actually differ. Each
/// needs two registers of its own (see [`resolve_attributes`]), and the fixed
/// palette plus all of them has to stay inside what a one-byte index addresses.
const FLASH_PAIRS: usize = 2 * 8 * 7;
const _: () = assert!(PALETTE_COLOURS + 2 * FLASH_PAIRS <= 256);

/// The [`CycleRange::rate`] value that means 60 cycle steps per second.
const CRNG_RATE_60HZ: u32 = 16384;
/// Interrupts — that is, frames — between the two halves of a FLASH cycle, and
/// the frame rate they are counted at. The ROM inverts a flashing cell every 16
/// of its 50Hz frames, so a full cycle takes just under two thirds of a second.
const FLASH_TOGGLE_FRAMES: u32 = 16;
const SPECTRUM_HZ: u32 = 50;
/// The same speed stated in CRNG's units, being what [`CycleRange`] speaks.
const FLASH_RATE: u16 = (CRNG_RATE_60HZ * SPECTRUM_HZ / (60 * FLASH_TOGGLE_FRAMES)) as u16;

/// One of the sixteen fixed colours, by palette register.
///
/// The three colour bits are wired straight to the ULA's outputs, so a register
/// is `bright:green:red:blue` with blue at the bottom — the reason the Spectrum
/// palette runs black, blue, red, magenta rather than through red first.
fn colour(register: u8) -> [u8; 3] {
    let level = if register & 8 != 0 {
        BRIGHT_LEVEL
    } else {
        NORMAL_LEVEL
    };
    let component = |bit: u8| if register & bit != 0 { level } else { 0 };
    [component(2), component(4), component(1)]
}

/// The palette registers an attribute byte's set and clear bits select. BRIGHT
/// applies to the whole cell, so it becomes the top bit of both registers.
fn ink_paper(attr: u8) -> (u8, u8) {
    let bright = (attr & BRIGHT) >> 3;
    ((attr & INK) | bright, ((attr & PAPER) >> 3) | bright)
}

/// Where a pixel row's 32 bytes begin in the bitmap.
///
/// The display file is addressed `010 t2t1 p2p1p0 r2r1r0 c4..c0`: the screen's
/// third, then the pixel row *within* a character cell, and only then the cell
/// row. Consecutive addresses therefore walk the eight rows of a cell only every
/// 256 bytes, which is what makes an unscrambled read of a `.SCR` come out as
/// eight interleaved combs.
fn row_offset(y: usize) -> usize {
    // The three fields of `y`, each moved to where the address wants it.
    ((y & 0xc0) << 5) | ((y & 0x07) << 8) | ((y & 0x38) << 2)
}

/// The palette, its cycling ranges, and the ink/paper registers each of the 256
/// possible attribute bytes resolves to.
///
/// Cells that flash cannot use the fixed sixteen registers, since a rotation
/// applied there would flash every cell sharing those colours. Each flashing
/// combination present in the picture instead gets a private pair of registers
/// with a two-entry cycle range over them, which rotating swaps — exactly the
/// exchange the ROM performs.
fn resolve_attributes(attrs: &[u8]) -> (Vec<[u8; 3]>, Vec<CycleRange>, [(u8, u8); 256]) {
    let mut palette: Vec<[u8; 3]> = (0..PALETTE_COLOURS as u8).map(colour).collect();
    let mut ranges = Vec::new();
    let mut registers = [(0u8, 0u8); 256];
    for (attr, entry) in registers.iter_mut().enumerate() {
        *entry = ink_paper(attr as u8);
    }

    // Driven by the attributes actually in the picture, so the register budget
    // is spent on cells that are really on screen.
    for &attr in attrs {
        let (ink, paper) = registers[attr as usize];
        if attr & FLASH == 0
            // Swapping a cell's colours is invisible when they are the same.
            || ink == paper
            // A pair past the fixed palette means this attribute is done.
            || ink as usize >= PALETTE_COLOURS
        {
            continue;
        }
        let base = palette.len() as u8;
        let (ink_rgb, paper_rgb) = (palette[ink as usize], palette[paper as usize]);
        palette.push(ink_rgb);
        palette.push(paper_rgb);
        ranges.push(CycleRange {
            low: base,
            high: base + 1,
            rate: FLASH_RATE,
            active: true,
            // Which way a two-entry range turns makes no difference.
            reverse: false,
        });
        registers[attr as usize] = (base, base + 1);
    }
    (palette, ranges, registers)
}

/// Decode an in-memory ZX Spectrum screen, keeping the palette and per-pixel
/// indices so FLASH can be applied at display time.
///
/// The length has to be exactly [`is_screen`]: with no header to check, size is
/// the only thing separating a screen dump from any other file of the same
/// length, so anything else is refused rather than shown as a garbled picture.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    ensure!(
        is_screen(bytes.len()),
        "not a ZX Spectrum screen: {} bytes, not {BITMAP_BYTES} or {FILE_BYTES}",
        bytes.len()
    );
    // A bitmap on its own is a legitimate dump — the two halves live in
    // different places in a Spectrum's memory map, and tools that only wanted
    // the picture saved only the first.
    const CLS_ATTRS: [u8; ATTR_BYTES] = [CLS_ATTR; ATTR_BYTES];
    let attrs = bytes
        .get(BITMAP_BYTES..BITMAP_BYTES + ATTR_BYTES)
        .unwrap_or(&CLS_ATTRS);
    let (palette, ranges, registers) = resolve_attributes(attrs);

    let mut indices = vec![0u8; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        let row = row_offset(y);
        for column in 0..ATTR_COLUMNS {
            let (ink, paper) = registers[attrs[y / 8 * ATTR_COLUMNS + column] as usize];
            let bits = bytes[row + column];
            for bit in 0..8 {
                // The leftmost pixel of a byte is its most significant bit.
                let set = bits & (0x80 >> bit) != 0;
                indices[y * WIDTH + column * 8 + bit] = if set { ink } else { paper };
            }
        }
    }

    Ok(IndexedImage {
        width: WIDTH as u32,
        height: HEIGHT as u32,
        palette,
        indices,
        ranges,
    })
}

#[allow(dead_code)]
/// Load a ZX Spectrum screen from a file (see [`load_indexed_from_memory`]).
pub fn load_indexed(path: impl AsRef<Path>) -> Result<IndexedImage> {
    load_indexed_from_memory(&fs::read(path.as_ref())?)
}

/// One-line description of the format for the frontend's info display. A `.SCR`
/// has no header, no palette and one fixed size, so there is nothing to read
/// out of the file: every screen is described the same way.
pub fn describe() -> String {
    format!("ZX Spectrum {WIDTH}x{HEIGHT} (SCR)")
}

/// Whether a file of this length can be a screen dump, which is as much as can
/// ever be told about one without looking at the picture: there is no header to
/// sniff, so only the full 6912 bytes, or a bare bitmap, are accepted. Anything
/// looser would claim files of other formats that happen to be big enough.
pub fn is_screen(len: usize) -> bool {
    len == FILE_BYTES || len == BITMAP_BYTES
}

#[cfg(test)]
#[path = "tests/zx_scr_tests.rs"]
mod tests;
