//! Atari ST still images: DEGAS (`.PI1`, `.PC1`), NEOchrome (`.NEO`),
//! CrackArt (`.CA1`) and Fullscreen Construction Kit (`.KID`).
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
//! The other formats a demo is likely to carry its loading picture in are the
//! same dump behind a different header, and are decoded here as well:
//!
//! * NEOchrome (`.NEO`) pads the header out to 128 bytes with the picture's
//!   original filename and one channel of colour animation, then stores the
//!   screen untouched. Its resolution word can say what DEGAS' does, but the
//!   program only ever drew in low resolution.
//! * CrackArt (`.CA1`, `.CA2`, `.CA3`) is the only one of the three with a
//!   signature, and the only one whose compression beats PackBits: see
//!   [`unpack_crackart`].
//! * Fullscreen Construction Kit (`.KID`) stores an overscanned screen — the
//!   borders opened up, so wider and taller than a framebuffer — behind DEGAS'
//!   palette and a two-byte magic. It is the odd one out here in being the
//!   only picture that is not 32000 bytes of screen memory.
//!
//! Images decode into the same [`IndexedImage`] the ILBM decoder produces, so
//! [`ImageEmu`](crate::image_emu::ImageEmu) animates a DEGAS Elite colour
//! animation — or a NEOchrome one — exactly like a DeluxePaint CRNG one.
//!
//! All four are recognised by content as well as by extension, and all three
//! screen modes decode — they differ only in how many planes the 32000 bytes
//! are split into — though what a demo ships is almost always low resolution.

use std::{fs, path::Path};

use anyhow::{Result, bail, ensure};

use crate::ilbm::{CycleRange, IndexedImage, scale_grid, unpack_byterun1};

/// Bytes of screen memory in an ST framebuffer. The same in every resolution:
/// fewer bitplanes buy proportionally more pixels.
const SCREEN_BYTES: usize = 32000;

/// DEGAS' fixed header: a resolution word plus sixteen palette words.
const HEADER_BYTES: usize = 34;

/// NEOchrome's header. The resolution and palette are followed by the
/// picture's original filename, the colour-animation settings and padding.
const NEO_HEADER_BYTES: usize = 128;

/// CrackArt's header ahead of the palette: the `CA` magic, a compression flag
/// and a resolution byte.
const CA_HEADER_BYTES: usize = 4;

/// Fullscreen Construction Kit's magic, in place of DEGAS' resolution word: the
/// palette follows it exactly as it does there.
const KID_MAGIC: &[u8; 2] = b"KD";

/// The one overscanned screen a `.KID` holds. Its scanlines are 230 bytes where
/// a low-resolution framebuffer's are 160, and there are 274 of them rather
/// than 200 — the borders opened top and bottom as well as at the sides.
const KID_WIDTH: usize = 448;
const KID_HEIGHT: usize = 274;
const KID_STRIDE: usize = 230;

/// The size every `.KID` file has, which is most of what identifies one.
const KID_BYTES: usize = HEADER_BYTES + KID_STRIDE * KID_HEIGHT;

/// The `.NEO` header words holding the colour animation (see
/// [`parse_neo_range`]).
const NEO_ANIM_LIMITS: usize = 48;
const NEO_ANIM_SPEED: usize = 50;

/// How much of a file [`is_st_image`] needs: enough for the longest header the
/// checks below read, which is NEOchrome's resolution word plus its palette.
pub const SNIFF_BYTES: usize = 36;

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

/// Which of the three formats a file is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Degas,
    Neo,
    CrackArt,
    Kid,
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
    /// Bytes from one scanline to the next. That is a word per plane for every
    /// sixteen pixels in a screen mode, but an overscanned KID line carries six
    /// bytes beyond the pixels this decodes.
    stride: usize,
}

/// The mode every `.KID` is in: low resolution with the borders opened.
const KID_MODE: Mode = Mode {
    width: KID_WIDTH,
    height: KID_HEIGHT,
    planes: 4,
    yscale: 1,
    stride: KID_STRIDE,
};

/// The mode a DEGAS resolution word selects (with the compression bit already
/// masked off). NEOchrome and CrackArt number their resolutions the same way.
fn mode_for(res: u16) -> Result<Mode> {
    let (width, height, planes, yscale) = match res {
        0 => (320, 200, 4, 1),
        // Medium-res pixels are half as wide as low-res ones, so the picture is
        // doubled vertically to make them square again.
        1 => (640, 200, 2, 2),
        // Monochrome; already square.
        2 => (640, 400, 1, 1),
        _ => bail!("not an ST picture: resolution word is {res}"),
    };
    Ok(Mode {
        width,
        height,
        planes,
        yscale,
        // A screen mode stores exactly the pixels it shows.
        stride: width / 8 * planes,
    })
}

/// The screen mode a file is in, read from wherever its format keeps the
/// resolution: the first word for DEGAS (whose top bit marks compression), the
/// second for NEOchrome, and the fourth byte for CrackArt. KID has no
/// resolution to read, having only ever written the one.
fn mode_of(bytes: &[u8], format: Format) -> Result<Mode> {
    let res = match format {
        Format::Kid => return Ok(KID_MODE),
        Format::Degas => {
            ensure!(bytes.len() >= 2, "too short to be an ST picture");
            be16(bytes, 0) & 0x7fff
        }
        Format::Neo => {
            ensure!(bytes.len() >= 4, "too short to be a NEOchrome picture");
            be16(bytes, 2)
        }
        Format::CrackArt => {
            ensure!(bytes.len() >= 4, "too short to be a CrackArt picture");
            bytes[3] as u16
        }
    };
    mode_for(res)
}

/// Palette words a CrackArt file stores: only the registers the mode can
/// actually show, and none at all in monochrome.
fn ca_palette_words(mode: Mode) -> usize {
    match mode.planes {
        1 => 0,
        planes => 1 << planes,
    }
}

/// Convert `count` palette words at `offset` into RGB.
///
/// A colour word is `0000 rrrr gggg bbbb`. The original ST only had three bits
/// per component, in bits 2-0 of each nibble; the STE added a fourth bit that
/// is *less* significant than those, in bit 3, making an STE component
/// `(n & 7) << 1 | n >> 3`. Reading an ST picture that way would darken it (its
/// white would come out at 238), so the two are told apart by whether any
/// component uses bit 3 at all.
fn parse_palette(data: &[u8], offset: usize, count: usize) -> Vec<[u8; 3]> {
    let words: Vec<u16> = (0..count)
        .map(|i| be16(data, offset + i * 2) & 0x0fff)
        .collect();
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
    /// Uncompressed (`.PI1`), and both NEOchrome and CrackArt, which store
    /// screen memory as the shifter reads it: one word from each plane in turn.
    Interleaved,
    /// Compressed (`.PC1`): each plane's whole scanline in turn.
    Sequential,
}

/// A file decoded as far as screen memory, before the planes are unwoven.
struct Picture {
    mode: Mode,
    palette: Vec<[u8; 3]>,
    planar: Vec<u8>,
    layout: Layout,
    ranges: Vec<CycleRange>,
}

/// Turn planar screen memory into one palette index per pixel (row-major).
fn decode_indices(planar: &[u8], mode: Mode, layout: Layout) -> Vec<u8> {
    let row_bytes = mode.width / 8;
    let mut out = vec![0u8; mode.width * mode.height];
    for y in 0..mode.height {
        let row = y * mode.stride;
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

/// Read NEOchrome's one channel of colour animation as a cycle range.
///
/// Two words say all there is to say about it. The limits word holds the first
/// and last register of the range in the nibbles of its low byte, and its top
/// bit says whether the animation was ever set up; the speed word's top bit is
/// what switches it on, and its low byte — read signed — is one more than the
/// number of vertical blanks between steps, negative for a leftwards cycle. A
/// count of zero is out of that reckoning, and taken as the fastest step there
/// is: one every vertical blank.
fn parse_neo_range(header: &[u8]) -> Vec<CycleRange> {
    let limits = be16(header, NEO_ANIM_LIMITS);
    let speed = be16(header, NEO_ANIM_SPEED);
    let (low, high) = ((limits >> 4) as u8 & 0xf, limits as u8 & 0xf);
    if limits & 0x8000 == 0 || low >= high {
        return Vec::new();
    }
    let steps = (speed as u8) as i8;
    let vblanks = (steps.unsigned_abs() as u32).saturating_sub(1).max(1);
    vec![CycleRange {
        low,
        high,
        rate: (CRNG_RATE_60HZ / vblanks) as u16,
        active: speed & 0x8000 != 0,
        reverse: steps < 0,
    }]
}

/// The screen being filled in by [`unpack_crackart`], which writes it in
/// CrackArt's order rather than from front to back.
struct Screen {
    data: Vec<u8>,
    /// Where the next byte goes.
    pos: usize,
    /// Where the walk resumed the last time it ran off the end, and so where
    /// the next one after that will start.
    start: usize,
    /// How far on from `pos` the byte after it goes.
    offset: usize,
    /// Bytes written so far, which is what says when the screen is full: `pos`
    /// wraps around and cannot.
    written: usize,
}

impl Screen {
    fn put(&mut self, value: u8, count: usize) {
        // `pos` is only ever `start` or a step on from it, and `start` never
        // outruns `written`, so the write below stays inside the screen.
        for _ in 0..count.min(SCREEN_BYTES - self.written) {
            self.data[self.pos] = value;
            self.pos += self.offset;
            self.written += 1;
            // Off the end and back to the top, one byte further along than the
            // last pass began — with an offset of a whole scanline that is what
            // starts the next column. Whatever the walk overshot by is dropped.
            if self.pos >= SCREEN_BYTES {
                self.start += 1;
                self.pos = self.start;
            }
        }
    }
}

/// Undo CrackArt's compression, given everything in the file after the palette.
///
/// The stream opens with the escape byte, a fill byte and the offset word, and
/// what follows is run-length encoding with two twists. The first is the
/// offset: consecutive bytes are not stored consecutively but that far apart,
/// starting again one byte further along each time the walk runs off the bottom
/// of the screen. Written a scanline at a time, as every file in the wild is,
/// this walks *down* the screen a column at a time, where a picture repeats
/// itself far more often than it does along a scanline. The second is the fill
/// byte, which the whole screen starts out holding, so a run of it need not be
/// stored — only stepped over, and the run that reaches the end of the picture
/// not even that.
///
/// A stream that ends early leaves the rest of the screen as the fill byte:
/// half a picture beats none.
fn unpack_crackart(src: &[u8]) -> Result<Vec<u8>> {
    ensure!(src.len() >= 4, "truncated CrackArt image");
    let (escape, delta) = (src[0], src[1]);
    let mut screen = Screen {
        data: vec![delta; SCREEN_BYTES],
        pos: 0,
        start: 0,
        // The top bit of the offset word is not part of the offset.
        offset: (be16(src, 2) & 0x7fff) as usize,
        written: 0,
    };

    let mut i = 4;
    while screen.written < SCREEN_BYTES {
        let Some(&cmd) = src.get(i) else { break };
        i += 1;
        if cmd != escape {
            screen.put(cmd, 1);
            continue;
        }
        let Some(&control) = src.get(i) else { break };
        i += 1;
        let (value, count) = match control {
            // The escape byte itself, when it turns up in the picture.
            c if c == escape => (escape, 1),
            // A run counted by a byte, then one counted by a word.
            0 => {
                let Some(run) = src.get(i..i + 2) else { break };
                i += 2;
                (run[1], run[0] as usize + 1)
            }
            1 => {
                let Some(run) = src.get(i..i + 3) else { break };
                i += 3;
                (run[2], be16(run, 0) as usize + 1)
            }
            // A run of the fill byte the screen already holds, so this only
            // steps over it. Its count is a word whose high byte is never zero:
            // a zero there means the fill runs to the end of the picture, which
            // the screen is already full of.
            2 => {
                let Some(run) = src.get(i..i + 2) else { break };
                i += 2;
                if run[0] == 0 {
                    break;
                }
                (delta, be16(run, 0) as usize + 1)
            }
            // The short form: the count is in the control byte, and the byte to
            // repeat follows it.
            c => {
                let Some(&value) = src.get(i) else { break };
                i += 1;
                (value, c as usize + 1)
            }
        };
        screen.put(value, count);
    }
    Ok(screen.data)
}

/// Decode a DEGAS `.PI1`/`.PC1` as far as screen memory.
fn load_degas(bytes: &[u8]) -> Result<Picture> {
    ensure!(
        bytes.len() > HEADER_BYTES,
        "too short to be a DEGAS image ({} bytes)",
        bytes.len()
    );
    // Bit 15 of the resolution word marks the compressed (Elite) variant.
    let compressed = be16(bytes, 0) & 0x8000 != 0;
    let mode = mode_of(bytes, Format::Degas)?;
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

    Ok(Picture {
        mode,
        palette: parse_palette(bytes, 2, 16),
        planar,
        layout,
        ranges: body
            .get(used..)
            .filter(|rest| rest.len() >= TRAILER_BYTES)
            .map(|rest| parse_ranges(&rest[..TRAILER_BYTES]))
            .unwrap_or_default(),
    })
}

/// Decode a NEOchrome `.NEO`: a longer header in front of the same raw screen.
fn load_neo(bytes: &[u8]) -> Result<Picture> {
    let mode = mode_of(bytes, Format::Neo)?;
    ensure!(
        bytes.len() >= NEO_HEADER_BYTES + SCREEN_BYTES,
        "truncated NEOchrome image: {} of {} bytes",
        bytes.len(),
        NEO_HEADER_BYTES + SCREEN_BYTES
    );
    Ok(Picture {
        mode,
        palette: parse_palette(bytes, 4, 16),
        planar: bytes[NEO_HEADER_BYTES..NEO_HEADER_BYTES + SCREEN_BYTES].to_vec(),
        layout: Layout::Interleaved,
        ranges: parse_neo_range(bytes),
    })
}

/// Decode a CrackArt `.CA1`/`.CA2`/`.CA3`, packed or not.
fn load_crackart(bytes: &[u8]) -> Result<Picture> {
    let mode = mode_of(bytes, Format::CrackArt)?;
    let words = ca_palette_words(mode);
    let body_at = CA_HEADER_BYTES + words * 2;
    ensure!(bytes.len() > body_at, "truncated CrackArt image");
    let palette = if words == 0 {
        // Monochrome stores no palette: the ST shows register 0 as white.
        vec![[255, 255, 255], [0, 0, 0]]
    } else {
        parse_palette(bytes, CA_HEADER_BYTES, words)
    };
    let planar = match bytes[2] {
        0 => {
            ensure!(
                bytes.len() >= body_at + SCREEN_BYTES,
                "truncated CrackArt image: {} of {SCREEN_BYTES} bytes of screen data",
                bytes.len() - body_at
            );
            bytes[body_at..body_at + SCREEN_BYTES].to_vec()
        }
        1 => unpack_crackart(&bytes[body_at..])?,
        flag => bail!("not a CrackArt image: compression flag is {flag}"),
    };
    Ok(Picture {
        mode,
        palette,
        planar,
        layout: Layout::Interleaved,
        // CrackArt has nowhere to put colour animation.
        ranges: Vec::new(),
    })
}

/// Decode a Fullscreen Construction Kit `.KID`: DEGAS' palette behind a magic
/// of its own, then an overscanned screen stored the way the shifter reads it.
fn load_kid(bytes: &[u8]) -> Result<Picture> {
    ensure!(
        bytes.len() >= KID_BYTES,
        "truncated KID image: {} of {KID_BYTES} bytes",
        bytes.len()
    );
    Ok(Picture {
        mode: KID_MODE,
        palette: parse_palette(bytes, 2, 16),
        planar: bytes[HEADER_BYTES..KID_BYTES].to_vec(),
        layout: Layout::Interleaved,
        // Nor has KID anywhere to put colour animation.
        ranges: Vec::new(),
    })
}

/// The format a file about to be decoded is in.
///
/// Looser than [`is_st_image`], which has to keep other formats out: by the
/// time a file gets here it is believed to be an ST picture — the extension may
/// have said so — and only the four have to be told apart. CrackArt and KID
/// have their magics; NEOchrome and an uncompressed DEGAS both open with a zero
/// word, and are separated by the size, which each is obliged to have exactly.
fn format_of(bytes: &[u8]) -> Format {
    if bytes.starts_with(b"CA") {
        Format::CrackArt
    } else if bytes.starts_with(KID_MAGIC) {
        Format::Kid
    } else if bytes.len() >= NEO_HEADER_BYTES + SCREEN_BYTES
        && be16(bytes, 0) == 0
        && mode_for(be16(bytes, 2)).is_ok()
    {
        Format::Neo
    } else {
        Format::Degas
    }
}

/// Decode an in-memory ST picture in any of the three formats, preserving the
/// palette and per-pixel indices so colour cycling can be applied at display
/// time.
pub fn load_indexed_from_memory(bytes: &[u8]) -> Result<IndexedImage> {
    let picture = match format_of(bytes) {
        Format::Degas => load_degas(bytes)?,
        Format::Neo => load_neo(bytes)?,
        Format::CrackArt => load_crackart(bytes)?,
        Format::Kid => load_kid(bytes)?,
    };
    let Picture {
        mode,
        palette,
        planar,
        layout,
        ranges,
    } = picture;
    let indices = decode_indices(&planar, mode, layout);
    Ok(IndexedImage {
        width: mode.width as u32,
        height: (mode.height * mode.yscale) as u32,
        palette,
        // Aspect correction replicates indices, not colours, so the palette
        // (and any cycling applied to it) is untouched.
        indices: scale_grid(&indices, mode.width, mode.height, 1, mode.yscale),
        ranges,
    })
}

/// One-line description of an ST picture's size and colour mode, for the
/// frontend's info display — e.g. `Atari 320x200 (16 colors)`. Only the
/// resolution is read, so it costs nothing. Best effort: a file whose header
/// says nothing useful is just called an Atari picture.
pub fn describe(bytes: &[u8]) -> String {
    // All four are named the same: which program wrote the file says nothing
    // about the picture.
    let name = "Atari";
    let Ok(mode) = mode_of(bytes, format_of(bytes)) else {
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
/// Load an ST picture from a file (see [`load_indexed_from_memory`]).
pub fn load_indexed(path: impl AsRef<Path>) -> Result<IndexedImage> {
    load_indexed_from_memory(&fs::read(path.as_ref())?)
}

/// Content sniff for a DEGAS file, for when the extension doesn't say. Only the
/// header is inspected — `data` may be a prefix of the file, with `len` its full
/// length — so this checks the resolution word, the size an uncompressed file is
/// obliged to have, and that every palette word leaves its top nibble clear the
/// way an ST colour does.
fn is_degas(data: &[u8], len: usize) -> bool {
    if data.len() < HEADER_BYTES {
        return false;
    }
    let res = be16(data, 0);
    if mode_for(res & 0x7fff).is_err() || !palette_looks_st(data, 2, 16) {
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

/// Content sniff for a NEOchrome file. Its header opens with a word of nothing
/// at all, so there is little to go on beyond the palette and the one size the
/// format has: header plus screen, never compressed and never with a trailer.
fn is_neo(data: &[u8], len: usize) -> bool {
    len == NEO_HEADER_BYTES + SCREEN_BYTES
        && data.len() >= SNIFF_BYTES
        && be16(data, 0) == 0
        && mode_for(be16(data, 2)).is_ok()
        && palette_looks_st(data, 4, 16)
}

/// Content sniff for a CrackArt file, which unlike the other two has a
/// signature — leaving only the size to check, and the palette that the
/// resolution says is there.
fn is_crackart(data: &[u8], len: usize) -> bool {
    if !data.starts_with(b"CA") || data.len() < CA_HEADER_BYTES {
        return false;
    }
    let Ok(mode) = mode_for(data[3] as u16) else {
        return false;
    };
    let words = ca_palette_words(mode);
    if data.len() < CA_HEADER_BYTES + words * 2 || !palette_looks_st(data, CA_HEADER_BYTES, words) {
        return false;
    }
    let body_at = CA_HEADER_BYTES + words * 2;
    match data[2] {
        0 => len == body_at + SCREEN_BYTES,
        // Compressed, and as with DEGAS a file that big would have been stored
        // plain instead.
        1 => (body_at..body_at + SCREEN_BYTES).contains(&len),
        _ => false,
    }
}

/// Content sniff for a KID file. Like CrackArt it has a signature, and unlike
/// any of the others it is never compressed, so there is exactly one size it
/// can have.
fn is_kid(data: &[u8], len: usize) -> bool {
    len == KID_BYTES
        && data.len() >= HEADER_BYTES
        && data.starts_with(KID_MAGIC)
        && palette_looks_st(data, 2, 16)
}

/// Whether `count` palette words at `offset` all leave their top nibble clear,
/// the way an ST colour does.
fn palette_looks_st(data: &[u8], offset: usize, count: usize) -> bool {
    (0..count).all(|i| be16(data, offset + i * 2) & 0xf000 == 0)
}

/// Content sniff for an ST picture in any of the four formats, for when the
/// extension doesn't say. `data` may be a prefix of the file — [`SNIFF_BYTES`]
/// of it is enough — with `len` its full length.
pub fn is_st_image(data: &[u8], len: usize) -> bool {
    is_crackart(data, len) || is_kid(data, len) || is_neo(data, len) || is_degas(data, len)
}

#[cfg(test)]
#[path = "tests/degas_tests.rs"]
mod tests;
