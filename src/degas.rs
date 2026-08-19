//! Atari ST DEGAS / DEGAS Elite still images (`.PI1`, `.PC1`).
//!
//! DEGAS is the ST's answer to the Amiga's ILBM (see [`crate::ilbm`]), but with
//! none of the chunk structure: a file is a raw dump of a screen mode. Two
//! bytes of resolution, sixteen palette words and 32000 bytes of screen memory
//! — the size of an ST framebuffer in every resolution — optionally followed by
//! DEGAS Elite's 32-byte colour-animation trailer.
//!
//! A `.PC1` holds the same picture with the bitmap PackBits-compressed. That
//! form also reorders it: an uncompressed file interleaves the bitplanes word
//! by word, the way the shifter fetches them, while a compressed one stores
//! each scanline one whole plane at a time.
//!
//! Images decode into the same [`IndexedImage`] the ILBM decoder produces, so
//! [`ImageEmu`](crate::image_emu::ImageEmu) animates a DEGAS Elite colour
//! animation exactly like a DeluxePaint CRNG one.
//!
//! Only the low-resolution `.PI1`/`.PC1` are recognised as DEGAS by the file
//! scanner, but the decoder handles all three screen modes, which differ only
//! in how many planes the 32000 bytes are split into.

use std::{fs, path::Path};

use anyhow::{Result, bail, ensure};

use crate::ilbm::{CycleRange, IndexedImage, scale_grid, unpack_byterun1};

/// Bytes of screen memory in an ST framebuffer. The same in every resolution:
/// fewer bitplanes buy proportionally more pixels.
const SCREEN_BYTES: usize = 32000;

/// The fixed header: a resolution word plus sixteen palette words.
const HEADER_BYTES: usize = 34;

/// DEGAS Elite's colour-animation trailer: four channels described by four
/// words each — see [`parse_ranges`].
const TRAILER_BYTES: usize = 32;
const ANIM_CHANNELS: usize = 4;

/// The [`CycleRange::rate`] value that means 60 cycle steps per second.
const CRNG_RATE_60HZ: u32 = 16384;

/// Vertical blanks between animation steps when the trailer's delay word is 0.
/// The word counts *down* from this, so 0 is the slowest setting.
const MAX_ANIM_DELAY: u32 = 128;

fn be16(data: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([data[offset], data[offset + 1]])
}

/// An ST screen mode.
#[derive(Clone, Copy)]
struct Mode {
    width: usize,
    height: usize,
    planes: usize,
    /// Vertical pixel replication that squares up the mode's pixels, as
    /// [`ilbm::display_scale`](crate::ilbm) does for Amiga hires screens.
    yscale: usize,
}

/// The mode a DEGAS resolution word selects (with the compression bit already
/// masked off).
fn mode_for(res: u16) -> Result<Mode> {
    Ok(match res {
        0 => Mode {
            width: 320,
            height: 200,
            planes: 4,
            yscale: 1,
        },
        // Medium-res pixels are half as wide as low-res ones, so the picture is
        // doubled vertically to make them square again.
        1 => Mode {
            width: 640,
            height: 200,
            planes: 2,
            yscale: 2,
        },
        // Monochrome; already square.
        2 => Mode {
            width: 640,
            height: 400,
            planes: 1,
            yscale: 1,
        },
        _ => bail!("not a DEGAS image: resolution word is {res}"),
    })
}

/// Convert the sixteen palette words of a header into RGB.
///
/// A colour word is `0000 rrrr gggg bbbb`. The original ST only had three bits
/// per component, in bits 2-0 of each nibble; the STE added a fourth bit that
/// is *less* significant than those, in bit 3, making an STE component
/// `(n & 7) << 1 | n >> 3`. Reading an ST picture that way would darken it (its
/// white would come out at 238), so the two are told apart by whether any
/// component uses bit 3 at all.
fn parse_palette(header: &[u8]) -> Vec<[u8; 3]> {
    let words: Vec<u16> = (0..16).map(|i| be16(header, 2 + i * 2) & 0x0fff).collect();
    let ste = words.iter().any(|w| w & 0x0888 != 0);
    words
        .iter()
        .map(|w| {
            let comp = |shift: u32| {
                let n = ((w >> shift) & 0xf) as u8;
                if ste {
                    let v = ((n & 7) << 1) | (n >> 3);
                    (v << 4) | v
                } else {
                    // Spread 3 bits over the full range, so 7 -> 255.
                    (n << 5) | (n << 2) | (n >> 1)
                }
            };
            [comp(8), comp(4), comp(0)]
        })
        .collect()
}

/// How a scanline's bitplanes are laid out in the file.
#[derive(Clone, Copy)]
enum Layout {
    /// Uncompressed (`.PI1`): one word from each plane in turn.
    Interleaved,
    /// Compressed (`.PC1`): each plane's whole scanline in turn.
    Sequential,
}

/// Turn planar screen memory into one palette index per pixel (row-major).
fn decode_indices(planar: &[u8], mode: Mode, layout: Layout) -> Vec<u8> {
    let row_bytes = mode.width / 8;
    let mut out = vec![0u8; mode.width * mode.height];
    for y in 0..mode.height {
        let row = y * row_bytes * mode.planes;
        for x in 0..mode.width {
            let mut index = 0u8;
            for p in 0..mode.planes {
                let byte = match layout {
                    // Planes interleave a word at a time, so a plane's two
                    // bytes sit `planes` words into the group holding the pixel.
                    Layout::Interleaved => row + (x / 16) * mode.planes * 2 + p * 2 + (x % 16) / 8,
                    Layout::Sequential => row + p * row_bytes + x / 8,
                };
                index |= ((planar[byte] >> (7 - (x % 8))) & 1) << p;
            }
            out[y * mode.width + x] = index;
        }
    }
    out
}

/// Read DEGAS Elite's colour-animation trailer as cycle ranges.
///
/// The trailer is four parallel arrays of one word per channel: the first and
/// last palette register of the range, the direction (0 left, 1 off, 2 right)
/// and the delay, where `128 - delay` is the number of vertical blanks between
/// steps. Channels that are switched off — or that were never set up, and so
/// are all zeroes — describe an empty range and are dropped.
fn parse_ranges(trailer: &[u8]) -> Vec<CycleRange> {
    (0..ANIM_CHANNELS)
        .filter_map(|c| {
            let low = be16(trailer, c * 2);
            let high = be16(trailer, (ANIM_CHANNELS + c) * 2);
            let direction = be16(trailer, (ANIM_CHANNELS * 2 + c) * 2);
            let delay = be16(trailer, (ANIM_CHANNELS * 3 + c) * 2);
            if low >= high || high > 15 || direction == 1 || direction > 2 {
                return None;
            }
            let vblanks = MAX_ANIM_DELAY.saturating_sub(delay as u32).max(1);
            Some(CycleRange {
                low: low as u8,
                high: high as u8,
                // CRNG states a speed where DEGAS states a delay: one step
                // every `vblanks` frames is `16384 / vblanks` in its units.
                rate: (CRNG_RATE_60HZ / vblanks) as u16,
                active: true,
                reverse: direction == 0,
            })
        })
        .collect()
}

/// Decode an in-memory DEGAS image, preserving the palette and per-pixel
/// indices so colour cycling can be applied at display time.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    ensure!(
        bytes.len() > HEADER_BYTES,
        "too short to be a DEGAS image ({} bytes)",
        bytes.len()
    );
    let res = be16(bytes, 0);
    // Bit 15 marks the compressed (DEGAS Elite) variant.
    let compressed = res & 0x8000 != 0;
    let mode = mode_for(res & 0x7fff)?;
    let body = &bytes[HEADER_BYTES..];

    // `used` is where the picture ends and any trailer begins — for a packed
    // body that is only known once it has been unpacked.
    let (planar, layout, used) = if compressed {
        let (data, used) = unpack_byterun1(body, SCREEN_BYTES)?;
        (data, Layout::Sequential, used)
    } else {
        (body.to_vec(), Layout::Interleaved, SCREEN_BYTES)
    };
    ensure!(
        planar.len() >= SCREEN_BYTES,
        "truncated DEGAS image: {} of {SCREEN_BYTES} bytes of screen data",
        planar.len()
    );

    let ranges = body
        .get(used..)
        .filter(|rest| rest.len() >= TRAILER_BYTES)
        .map(|rest| parse_ranges(&rest[..TRAILER_BYTES]))
        .unwrap_or_default();

    let indices = decode_indices(&planar, mode, layout);
    Ok(IndexedImage {
        width: mode.width as u32,
        height: (mode.height * mode.yscale) as u32,
        palette: parse_palette(bytes),
        // Aspect correction replicates indices, not colours, so the palette
        // (and any cycling applied to it) is untouched.
        indices: scale_grid(&indices, mode.width, mode.height, 1, mode.yscale),
        ranges,
    })
}

/// One-line description of a DEGAS image's format, size and colour mode, for
/// the frontend's info display — e.g. `Atari DEGAS 320x200 (16 colors)`. Only
/// the resolution word is read, so it costs nothing. Best effort: a file whose
/// header says nothing useful is just called a DEGAS image.
pub fn describe(bytes: &[u8]) -> String {
    if bytes.len() < 2 {
        return "Atari".into();
    }
    let res = be16(bytes, 0);
    // Both the plain and the compressed (DEGAS Elite) variant are named the
    // same: which program wrote the file says nothing about the picture.
    let name = "Atari";
    let Ok(mode) = mode_for(res & 0x7fff) else {
        return name.into();
    };
    format!(
        "{name} {}x{} ({} colors)",
        mode.width,
        mode.height * mode.yscale,
        1 << mode.planes
    )
}

#[allow(dead_code)]
/// Load a DEGAS image from a file (see [`load_indexed_from_memory`]).
pub fn load_indexed(path: impl AsRef<Path>) -> Result<IndexedImage> {
    load_indexed_from_memory(&fs::read(path.as_ref())?)
}

/// Content sniff for a DEGAS file, for when the extension doesn't say. Only the
/// header is inspected — `data` may be a prefix of the file, with `len` its full
/// length — so this checks the resolution word, the size an uncompressed file is
/// obliged to have, and that every palette word leaves its top nibble clear the
/// way an ST colour does.
pub fn is_degas(data: &[u8], len: usize) -> bool {
    if data.len() < HEADER_BYTES {
        return false;
    }
    let res = be16(data, 0);
    if mode_for(res & 0x7fff).is_err() || (0..16).any(|i| be16(data, 2 + i * 2) & 0xf000 != 0) {
        return false;
    }
    if res & 0x8000 != 0 {
        // Compressed: the packed size is anyone's guess, but packing that
        // expands a screen means the writer would have stored it plain.
        (HEADER_BYTES..HEADER_BYTES + SCREEN_BYTES).contains(&len)
    } else {
        len == HEADER_BYTES + SCREEN_BYTES || len == HEADER_BYTES + SCREEN_BYTES + TRAILER_BYTES
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Paths here are rooted at the crate directory rather than left relative:
    /// a conversion running in another test switches the process-wide working
    /// directory for its duration (see `cbmconvert::CwdGuard`).
    fn get_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata/degas")
            .join(name)
    }

    /// Both variants decode to a low-res screen that uses its whole palette —
    /// a plane mix-up would still fill the frame, but a layout mix-up between
    /// the two would not agree on the picture, which the next test checks.
    #[test]
    fn decodes_both_variants() {
        for file in ["FUSE.PI1", "BOLEK3.PC1"] {
            let img = load_indexed(get_path(file)).unwrap();
            assert_eq!((img.width, img.height), (320, 200), "wrong size for {file}");
            assert_eq!(img.palette.len(), 16);
            assert_eq!(img.indices.len(), 320 * 200);
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
        let pal = parse_palette(&header);
        assert_eq!(pal[0], [255, 255, 255]);
        assert_eq!(pal[1], [255, 0, 0]);
        assert_eq!(pal[2], [0, 0, 0]);

        // One entry using bit 3 switches the whole palette to STE, where 0xf
        // is white and 0x7 is the value just below middle.
        header[6..8].copy_from_slice(&0x0fffu16.to_be_bytes());
        let pal = parse_palette(&header);
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

    #[test]
    fn sniffing() {
        for file in ["FUSE.PI1", "BOLEK3.PC1"] {
            let bytes = fs::read(get_path(file)).unwrap();
            assert!(is_degas(&bytes[..0x200], bytes.len()), "{file} not sniffed");
        }
        // Right shape, wrong size for an uncompressed screen.
        let bytes = fs::read(get_path("FUSE.PI1")).unwrap();
        assert!(!is_degas(&bytes[..0x200], bytes.len() - 1));
        // A palette word with a set top nibble is not an ST colour.
        let mut bad = bytes.clone();
        bad[2] = 0x10;
        assert!(!is_degas(&bad[..0x200], bad.len()));
        // Resolution 3 does not exist.
        assert!(!is_degas(&[0, 3], 32034));
        assert!(load_indexed_from_memory(&[0, 3]).is_err());
    }
}
