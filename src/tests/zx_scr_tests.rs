use super::*;

/// A screen filled with `bits` in every bitmap byte and `attr` in every
/// attribute cell.
fn flat_screen(bits: u8, attr: u8) -> Vec<u8> {
    let mut bytes = vec![bits; BITMAP_BYTES];
    bytes.resize(FILE_BYTES, attr);
    bytes
}

/// The display file's row order: thirds of the screen, then the pixel row
/// within a character cell, then the cell row. Every row must land on a
/// distinct 32-byte slot inside the bitmap.
#[test]
fn row_addressing() {
    assert_eq!(row_offset(0), 0);
    // The next address along is the same cell row, one pixel row down.
    assert_eq!(row_offset(1), 256);
    // A whole character row is 32 bytes on from the previous one.
    assert_eq!(row_offset(8), 32);
    // Each third is a 2048-byte block of its own.
    assert_eq!(row_offset(64), 2048);
    assert_eq!(row_offset(128), 4096);
    assert_eq!(row_offset(191), BITMAP_BYTES - 32);

    let mut rows: Vec<usize> = (0..HEIGHT).map(row_offset).collect();
    rows.sort();
    rows.dedup();
    assert_eq!(rows.len(), HEIGHT, "two rows share a slot");
    assert!(rows.iter().all(|r| r % 32 == 0));
}

/// Ink, paper and BRIGHT as the ULA reads them, with blue as the lowest
/// colour bit.
#[test]
fn attributes_and_palette() {
    // Bright yellow ink (green + red) on blue paper.
    let (ink, paper) = ink_paper(BRIGHT | 0x06 | (1 << 3));
    assert_eq!((ink, paper), (0x0e, 0x09));
    assert_eq!(colour(ink), [255, 255, 0]);
    assert_eq!(colour(paper), [0, 0, 255]);

    // The same colours without BRIGHT stop short of full intensity.
    let (ink, paper) = ink_paper(0x06 | (1 << 3));
    assert_eq!(colour(ink), [0xd7, 0xd7, 0]);
    assert_eq!(colour(paper), [0, 0, 0xd7]);

    assert_eq!(colour(0), [0, 0, 0]);
    // Black is black at either brightness, and BRIGHT black is a real
    // attribute — hence sixteen registers for fifteen distinct colours.
    assert_eq!(colour(8), [0, 0, 0]);
    assert_eq!(colour(7), [0xd7, 0xd7, 0xd7]);
}

/// Set bits take the cell's ink and clear bits its paper, at the position
/// the scrambled row order puts them.
#[test]
fn decodes_pixels_and_colours() {
    // Red ink on cyan paper, and a bitmap of alternating pixels.
    let attr = 0x02 | (0x05 << 3);
    let img = load_indexed_from_memory(&flat_screen(0b1010_1010, attr)).unwrap();
    assert_eq!((img.width, img.height), (256, 192));
    assert_eq!(img.palette.len(), PALETTE_COLOURS);
    assert!(
        img.ranges.is_empty(),
        "nothing flashes without the FLASH bit"
    );
    let (ink, paper) = (0x02, 0x05);
    assert!(
        img.indices
            .chunks_exact(2)
            .all(|px| px == [ink, paper].as_slice())
    );

    // One byte set, in the last row of the first character row: pixel row 7
    // of the top third, at cell column 3.
    let mut bytes = flat_screen(0, attr);
    bytes[row_offset(7) + 3] = 0xff;
    let img = load_indexed_from_memory(&bytes).unwrap();
    let lit: Vec<usize> = img
        .indices
        .iter()
        .enumerate()
        .filter(|(_, index)| **index == ink)
        .map(|(at, _)| at)
        .collect();
    assert_eq!(lit, (7 * WIDTH + 24..7 * WIDTH + 32).collect::<Vec<_>>());
}

/// A flashing cell gets a private pair of registers and a two-entry cycle
/// range that swaps them; cells sharing an attribute share the pair.
#[test]
fn flash_becomes_a_cycle_range() {
    // Magenta ink on green paper, flashing.
    let attr = FLASH | 0x03 | (0x04 << 3);
    let img = load_indexed_from_memory(&flat_screen(0b1111_0000, attr)).unwrap();
    assert_eq!(img.ranges.len(), 1, "one attribute, one range");
    let range = img.ranges[0];
    assert_eq!((range.low, range.high), (16, 17));
    assert_eq!(range.rate, FLASH_RATE);
    assert!(range.active);
    // The pair holds the cell's own colours, not the fixed registers'.
    assert_eq!(img.palette[16], colour(0x03));
    assert_eq!(img.palette[17], colour(0x04));
    assert_eq!(img.palette.len(), PALETTE_COLOURS + 2);
    // Every cell in the picture has the same attribute, so every pixel must
    // have been pointed at that one pair.
    assert!(img.indices.iter().all(|&i| i == 16 || i == 17));

    // A flashing cell whose colours match has nothing to swap.
    let same =
        load_indexed_from_memory(&flat_screen(0xff, FLASH | 0x02 | (0x02 << 3))).unwrap();
    assert!(same.ranges.is_empty());
    assert_eq!(same.palette.len(), PALETTE_COLOURS);
}

/// Every flashing combination the picture uses gets its own pair, and the
/// worst case — all of them at once — still fits in a byte-wide index.
#[test]
fn flash_register_budget() {
    // The 128 flashing attributes, which is every combination there is.
    let mut bytes = vec![0xaa; BITMAP_BYTES];
    bytes.extend((0..ATTR_BYTES).map(|i| FLASH | (i % 128) as u8));
    let img = load_indexed_from_memory(&bytes).unwrap();
    // Sixteen of the 128 have equal ink and paper and take no registers.
    assert_eq!(img.ranges.len(), FLASH_PAIRS);
    assert_eq!(img.palette.len(), PALETTE_COLOURS + 2 * FLASH_PAIRS);
    assert!(
        img.indices
            .iter()
            .all(|&i| (i as usize) < img.palette.len())
    );
    assert!(
        img.ranges
            .iter()
            .all(|r| (r.high as usize) < img.palette.len())
    );
}

/// A bitmap saved without its attributes shows as white on black.
#[test]
fn bitmap_without_attributes() {
    let img = load_indexed_from_memory(&vec![0b1100_0000; BITMAP_BYTES]).unwrap();
    assert_eq!((img.width, img.height), (256, 192));
    assert_eq!(&img.indices[..4], &[7, 7, 0, 0]);
}

/// Only the two exact sizes are a screen, and the decoder holds to that: a
/// file merely long enough is some other format, not a screen with a tail.
#[test]
fn recognised_sizes() {
    assert!(is_screen(FILE_BYTES));
    assert!(is_screen(BITMAP_BYTES));
    assert!(!is_screen(FILE_BYTES + 1));
    assert!(!is_screen(BITMAP_BYTES + 1));
    assert!(!is_screen(0));
    for len in [0, BITMAP_BYTES - 1, BITMAP_BYTES + 1, FILE_BYTES + 1] {
        assert!(
            load_indexed_from_memory(&vec![0; len]).is_err(),
            "{len} bytes was decoded as a screen"
        );
    }
}
