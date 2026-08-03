//! Static image backend that presents a decoded still image through the same
//! frontend plumbing as an emulator core.
//!
//! [`ImageEmu`] implements [`RetroEmu`] so an image file slots into the exact
//! same pipeline as a libretro core or the Flash backend: it hands the frontend
//! one RGBA frame via [`with_frame`](RetroEmu::with_frame) and reports its
//! geometry, while every interactive method (input, disks, audio, reset) is a
//! no-op. It decodes the Amiga ILBM/IFF format (see [`crate::ilbm`]) as well as
//! the common still formats handled by the `image` crate (PNG, BMP, JPEG).
//!
//! ILBM images can define colour-cycling ranges (CRNG chunks). When cycling is
//! enabled (opt-in via `--color-cycle`) the image is kept in its paletted form
//! and the RGBA frame is regenerated in [`run`](RetroEmu::run) from a palette
//! that is rotated according to how many frames have elapsed, animating the
//! picture the way DeluxePaint did.

use std::path::Path;

use anyhow::Result;

use crate::ilbm::{self, CycleRange};
use crate::retro_emu::Backend;

/// The CRNG `rate` value that corresponds to 60 cycle steps per second.
const CRNG_RATE_60HZ: f64 = 16384.0;

/// Frames per second the frontend is expected to call [`run`](RetroEmu::run)
/// at. The colour-cycling animation advances one frame per call, so this is
/// both the reported [`fps`](RetroEmu::fps) and the time base for cycling.
const FRAME_RATE: f64 = 60.0;

/// Decode a still image (PNG, BMP, JPEG, …) into an RGBA frame via the `image`
/// crate. The format is sniffed from the file contents rather than trusting the
/// extension, so a mis-named file still decodes.
fn load_image(game: &Path) -> Result<image::RgbaImage> {
    let img = image::ImageReader::open(game)?
        .with_guessed_format()?
        .decode()?;
    Ok(img.into_rgba8())
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
}

impl ImageEmu {
    pub fn new(game: &Path) -> Result<Self> {
        match ilbm::load_indexed(game) {
            Ok(img) => {
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
                };
                // Render the initial (unrotated) frame so the first presented
                // frame is correct even before any time has elapsed.
                emu.render(0.0);
                emu.last_offsets = emu.cycle_offsets(0.0);
                Ok(emu)
            }
            Err(_) => {
                // Not an indexed ILBM. Try the full IFF decoder (HAM, deep,
                // dynamic-palette, …); if that also fails the file isn't IFF at
                // all, so fall back to the `image` crate for PNG/BMP/JPEG.
                let img = ilbm::load(game).or_else(|_| load_image(game))?;
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
            // matching what the frontend uploads (see `retro_emu::frame_bytes`).
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
        0
    }
    fn reset(&mut self) {}
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn skip_frames(&mut self, _frames: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Paths here are rooted at the crate directory rather than left relative:
    /// a conversion running in another test switches the process-wide working
    /// directory for its duration (see `cbmconvert::CwdGuard`).
    fn root(rel: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
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

    /// PNG/BMP/JPEG still images decode through the same path as ILBM, via the
    /// `image` crate fallback. Each format is round-tripped through a temp file.
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
        for ext in ["png", "bmp", "jpg"] {
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
}
