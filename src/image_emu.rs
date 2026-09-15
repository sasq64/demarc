//! Static image backend that presents a decoded still image through the same
//! frontend plumbing as an emulator core.
//!
//! [`ImageEmu`] implements [`RetroEmu`] so an image file slots into the exact
//! same pipeline as a libretro core or the Flash backend: it hands the frontend
//! one RGBA frame via [`with_frame`](RetroEmu::with_frame) and reports its
//! geometry, while every interactive method (input, disks, audio, reset) is a
//! no-op. It decodes the Amiga ILBM/IFF format (see [`crate::ilbm`]), the Atari
//! ST DEGAS format (see [`crate::degas`]), ZX Spectrum screen dumps (see
//! [`crate::zx_scr`]) and palette-colour TIFF (see [`crate::tiff_pal`]), as
//! well as the common still formats handled by the `image` crate (PNG, BMP,
//! JPEG, TGA, PCX).
//!
//! Paletted images can define colour-cycling ranges (ILBM CRNG chunks, DEGAS
//! Elite colour animation, ZX Spectrum FLASH attributes). When cycling is
//! enabled (opt-in via `--color-cycle`) the image is kept in its paletted form
//! and the RGBA frame is regenerated in [`run`](RetroEmu::run) from a palette
//! that is rotated according to how many frames have elapsed, animating the
//! picture the way DeluxePaint did.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use crate::backend::Backend;
use crate::degas;
use crate::ilbm::{self, CycleRange};
use crate::tiff_pal;
use crate::utils::get_ext;
use crate::zx_scr;

/// The CRNG `rate` value that corresponds to 60 cycle steps per second.
const CRNG_RATE_60HZ: f64 = 16384.0;

/// Frames per second the frontend is expected to call [`run`](RetroEmu::run)
/// at. The colour-cycling animation advances one frame per call, so this is
/// both the reported [`fps`](RetroEmu::fps) and the time base for cycling.
const FRAME_RATE: f64 = 60.0;

/// Largest colour count [`load_image`] reports in place of a bit depth; past
/// this a truecolour image is described by its depth as before.
const MAX_COUNTED_COLORS: usize = 256;

/// Decode a still image (PNG, BMP, JPEG, …) into an RGBA frame via the `image`
/// crate, along with a one-line description of it for [`Backend::get_info`].
/// The format is sniffed from the file contents rather than trusting the
/// extension, so a mis-named file still decodes — except for TGA, which has no
/// signature to sniff and so falls back to the extension the reader was opened
/// with.
fn load_image(game: &Path) -> Result<(image::RgbaImage, String)> {
    // Teaches the `image` crate to decode PCX, both by extension and by
    // signature. Idempotent and internally guarded by a `Once`, so calling it
    // per load keeps it next to the code that needs it.
    image_extras::register();
    let reader = image::ImageReader::open(game)?.with_guessed_format()?;
    // Name the format while the reader still exists; decoding consumes it. The
    // first extension of the sniffed format is its common name (PNG, JPG, …),
    // and where nothing was sniffed the file's own extension is the best guess.
    let name = reader
        .format()
        .and_then(|f| f.extensions_str().first().copied())
        .map(str::to_uppercase)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| match get_ext(game).to_uppercase() {
            ext if ext.is_empty() => "Image".into(),
            ext => ext,
        });
    let img = reader.decode()?;
    // The depth of the decoded pixels rather than of the file: a paletted image
    // arrives here already expanded.
    let bits = img.color().bits_per_pixel();
    let rgba = img.into_rgba8();
    // A truecolour file that in fact uses few colours — pixel art saved as a
    // PNG, a converted screenshot — says more about itself with that count
    // than with the depth it happens to be stored at.
    let depth = match bits {
        24 | 32 => count_colors(&rgba, MAX_COUNTED_COLORS)
            .map(|n| match n {
                1 => "1 color".to_string(),
                n => format!("{n} colors"),
            })
            // Past the count worth reporting the two truecolour depths are the
            // same picture to look at, so they are named the same.
            .or(Some("True color".to_string())),
        _ => None,
    }
    .unwrap_or_else(|| format!("{bits}-bit"));
    let info = format!("{name} {}x{} ({depth})", rgba.width(), rgba.height());
    Ok((rgba, info))
}

/// Number of distinct colours in `img`, or `None` as soon as more than `limit`
/// of them have been seen — the count is only wanted while it stays small, so
/// there is no reason to keep tallying a photograph's thousands.
fn count_colors(img: &image::RgbaImage, limit: usize) -> Option<usize> {
    let mut seen: HashSet<u32> = HashSet::new();
    for px in img.pixels() {
        seen.insert(u32::from_ne_bytes(px.0));
        if seen.len() > limit {
            return None;
        }
    }
    Some(seen.len())
}

/// Presents a single decoded image as a "frame". For colour-cycling images the
/// frame is refreshed from a rotated palette on each [`run`](RetroEmu::run).
pub struct ImageEmu {
    width: usize,
    height: usize,
    /// Current RGBA8 frame (alpha opaque), updated by `run` while cycling.
    frame: Vec<u32>,
    /// Base palette (RGB); empty when there is nothing to cycle.
    palette: Vec<[u8; 3]>,
    /// One palette index per pixel; empty when there is nothing to cycle.
    indices: Vec<u8>,
    /// Active, in-bounds cycling ranges. Empty means the frame is static.
    ranges: Vec<CycleRange>,
    /// Number of `run` calls so far; the cycling clock, one step per frame.
    frames: u64,
    /// Per-range step offset last rendered, used to skip redundant redraws.
    last_offsets: Vec<i64>,
    /// Bumped by each redraw. Starts at 1 because both constructors leave a
    /// rendered frame behind. See [`Backend::frame_serial`].
    serial: u64,
    /// What the file turned out to be, as shown by [`Backend::get_info`].
    info: String,
}

impl ImageEmu {
    pub fn new(game: &Path) -> Result<Self> {
        // Read once: every decoder below is offered the same bytes, and so is
        // the format description that `get_info` hands back.
        let bytes = fs::read(game)?;
        // A ZX screen is identified by its size alone (see [`crate::zx_scr`]),
        // which a file of another format can hit by chance — so unlike the
        // formats below it, it is only decoded when the name says so too.
        let zx = |bytes: &[u8]| match get_ext(game).as_str() {
            "scr" => zx_scr::load_indexed_from_memory(bytes),
            ext => bail!("not a ZX Spectrum screen: extension is {ext:?}"),
        };
        // The ST's picture formats have next to no signature either — DEGAS
        // opens with a resolution word followed by a palette, which another
        // format's first 34 bytes can pass for (a 32-bit TGA opens with two
        // zero bytes, which read as a valid low-res word), and NEOchrome opens
        // with a word of nothing at all. The decoders themselves only check
        // that there is enough data, so they would happily turn the head of any
        // large file into a screenful of noise; the full sniff, which also
        // weighs the palette nibbles and the exact file size, is what keeps
        // them to real pictures.
        let atari = |bytes: &[u8]| {
            // DEGAS and CrackArt in each of their three resolutions, plus
            // NEOchrome and Fullscreen Construction Kit, which only ever saved
            // low-res pictures.
            const EXTENSIONS: [&str; 11] = [
                "pi1", "pi2", "pi3", "pc1", "pc2", "pc3", "ca1", "ca2", "ca3", "neo", "kid",
            ];
            let named = EXTENSIONS.contains(&get_ext(game).as_str());
            if !named && !degas::is_st_image(bytes, bytes.len()) {
                bail!("not an Atari ST picture");
            }
            degas::load_indexed_from_memory(bytes)
        };
        // Every paletted format lands in the same indexed representation, so
        // colour cycling works the same for an Amiga CRNG, a DEGAS Elite or
        // NEOchrome colour animation and a ZX Spectrum's flashing attributes.
        // Each decoder is paired with its own description of the file, so the
        // one that wins names the format `get_info` reports.
        match ilbm::load_indexed_from_memory(&bytes)
            .map(|img| (img, ilbm::describe(&bytes)))
            .or_else(|_| atari(&bytes).map(|img| (img, degas::describe(&bytes))))
            .or_else(|_| zx(&bytes).map(|img| (img, zx_scr::describe())))
            // A palette TIFF is here for a different reason: the `image` crate
            // refuses it outright, so this is the only decoder for it. Its
            // signature is a real one, so unlike the two above it needs no
            // help from the file's name.
            .or_else(|_| {
                tiff_pal::load_indexed_from_memory(&bytes)
                    .map(|img| (img, tiff_pal::describe(&bytes)))
            }) {
            Ok((img, info)) => {
                let width = img.width as usize;
                let height = img.height as usize;
                // Keep only ranges that actually animate and stay within the
                // palette; anything else is left untouched (a fixed colour).
                // With cycling disabled the list is empty and the frame stays
                // static.
                let ranges: Vec<CycleRange> = img
                    .ranges
                    .into_iter()
                    .filter(|r| {
                        r.active
                            && r.rate > 0
                            && r.high > r.low
                            && (r.high as usize) < img.palette.len()
                    })
                    .collect();
                let mut emu = Self {
                    width,
                    height,
                    frame: vec![0u32; width * height],
                    palette: img.palette,
                    indices: img.indices,
                    ranges,
                    frames: 0,
                    last_offsets: Vec::new(),
                    serial: 1,
                    info,
                };
                // Render the initial (unrotated) frame so the first presented
                // frame is correct even before any time has elapsed.
                emu.render(0.0);
                emu.last_offsets = emu.cycle_offsets(0.0);
                Ok(emu)
            }
            Err(_) => {
                // Not an indexed ILBM or DEGAS. Try the full IFF decoder (HAM, deep,
                // dynamic-palette, …); if that also fails the file isn't IFF at
                // all, so fall back to the `image` crate for PNG/BMP/JPEG.
                let (img, info) = match ilbm::load_from_memory(&bytes) {
                    Ok(img) => (img, ilbm::describe(&bytes)),
                    Err(_) => load_image(game)?,
                };
                let (w, h) = img.dimensions();
                Ok(Self {
                    width: w as usize,
                    height: h as usize,
                    frame: img
                        .into_raw()
                        .chunks_exact(4)
                        .map(|px| u32::from_ne_bytes([px[0], px[1], px[2], px[3]]))
                        .collect(),
                    palette: Vec::new(),
                    indices: Vec::new(),
                    ranges: Vec::new(),
                    frames: 0,
                    last_offsets: Vec::new(),
                    serial: 1,
                    info,
                })
            }
        }
    }

    /// Cycle step offset for each range at `elapsed` seconds, reduced modulo the
    /// range length (so equal vectors mean an identical-looking frame).
    fn cycle_offsets(&self, elapsed: f64) -> Vec<i64> {
        self.ranges
            .iter()
            .map(|r| {
                let n = (r.high - r.low) as i64 + 1;
                let steps_per_sec = r.rate as f64 * 60.0 / CRNG_RATE_60HZ;
                ((elapsed * steps_per_sec).floor() as i64).rem_euclid(n)
            })
            .collect()
    }

    /// Rebuild `frame` from the base palette rotated by the cycle offsets at
    /// `elapsed` seconds.
    fn render(&mut self, elapsed: f64) {
        let mut palette = self.palette.clone();
        for r in &self.ranges {
            let n = (r.high - r.low) as i64 + 1;
            let steps_per_sec = r.rate as f64 * 60.0 / CRNG_RATE_60HZ;
            let offset = (elapsed * steps_per_sec).floor() as i64;
            // Reverse just flips which way the colours travel through the range.
            let shift = if r.reverse { -offset } else { offset };
            for rel in 0..n {
                let src = (rel - shift).rem_euclid(n) as usize;
                palette[r.low as usize + rel as usize] = self.palette[r.low as usize + src];
            }
        }
        for (i, &idx) in self.indices.iter().enumerate() {
            let c = palette.get(idx as usize).copied().unwrap_or([0, 0, 0]);
            // Packed so the pixel's bytes land in `[r, g, b, a]` memory order,
            // matching what the frontend uploads (see `backend::frame_bytes`).
            self.frame[i] = u32::from_ne_bytes([c[0], c[1], c[2], 255]);
        }
    }
}

impl Backend for ImageEmu {
    // Advance the colour-cycling animation. A static image (no active ranges)
    // leaves the frame untouched; otherwise the frame is only rebuilt when the
    // rotation has actually moved.
    fn run(&mut self) -> bool {
        // Time is derived from the frame count on the assumption that the
        // frontend calls `run` at `FRAME_RATE`; no wall clock is consulted.
        self.frames += 1;
        let elapsed = self.frames as f64 / FRAME_RATE;
        let offsets = self.cycle_offsets(elapsed);
        if offsets != self.last_offsets {
            self.render(elapsed);
            self.last_offsets = offsets;
            self.serial += 1;
        }
        true
    }

    fn frame_hash(&self) -> u64 {
        self.serial
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(self.width, self.height, &self.frame);
    }

    // No audio; the frontend pushes nothing when the callback stays empty.
    fn with_audio(&mut self, _f: &mut dyn FnMut(&[i16])) {}

    fn get_frame_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn aspect_ratio(&self) -> f32 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    fn sample_rate(&self) -> f64 {
        0.0
    }

    // A nominal rate keeps the frontend's frame pacing happy; it also sets how
    // often `run` is called, i.e. the colour-cycling refresh rate.
    fn fps(&self) -> f64 {
        FRAME_RATE
    }

    // Everything below is inapplicable to a still image.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        1
    }
    fn reset(&mut self) {}
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn skip_frames(&mut self, _frames: u32) {}

    // What the picture is, rather than what it depicts: the format it was
    // decoded from, its displayed size and its colour mode.
    fn get_info(&self) -> Option<String> {
        Some(self.info.clone())
    }
}

#[cfg(test)]
#[path = "tests/image_emu_tests.rs"]
mod tests;
