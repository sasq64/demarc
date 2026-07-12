//! Static image backend that presents a decoded still image through the same
//! frontend plumbing as an emulator core.
//!
//! [`ImageEmu`] implements [`RetroEmu`] so an image file slots into the exact
//! same pipeline as a libretro core or the Flash backend: it hands the frontend
//! one RGBA frame via [`with_frame`](RetroEmu::with_frame) and reports its
//! geometry, while every interactive method (input, disks, audio, reset) is a
//! no-op. It decodes the Amiga ILBM/IFF format (see [`crate::ilbm`]).
//!
//! ILBM images can define colour-cycling ranges (CRNG chunks). When cycling is
//! enabled (opt-in via `--color-cycle`) the image is kept in its paletted form
//! and the RGBA frame is regenerated in [`run`](RetroEmu::run) from a palette
//! that is rotated according to elapsed wall-clock time, animating the picture
//! the way DeluxePaint did. Without the flag the image is presented static.

use std::path::Path;
use std::time::Instant;

use anyhow::Result;

use crate::ilbm::{self, CycleRange};
use crate::retro_emu::RetroEmu;

/// The CRNG `rate` value that corresponds to 60 cycle steps per second.
const CRNG_RATE_60HZ: f64 = 16384.0;

/// Presents a single decoded image as a "frame". For colour-cycling images the
/// frame is refreshed from a rotated palette on each [`run`](RetroEmu::run).
pub struct ImageEmu {
    width: usize,
    height: usize,
    /// Current RGBA8 frame (alpha opaque), updated by `run` while cycling.
    frame: Vec<u8>,
    /// Base palette (RGB); empty when there is nothing to cycle.
    palette: Vec<[u8; 3]>,
    /// One palette index per pixel; empty when there is nothing to cycle.
    indices: Vec<u8>,
    /// Active, in-bounds cycling ranges. Empty means the frame is static.
    ranges: Vec<CycleRange>,
    /// Time origin for the cycling animation.
    start: Instant,
    /// Per-range step offset last rendered, used to skip redundant redraws.
    last_offsets: Vec<i64>,
}

impl ImageEmu {
    pub fn new(game: &Path) -> Result<Self> {
        // Colour cycling is opt-in via `--color-cycle`; without it the image is
        // presented static even when it carries CRNG ranges.
        // Prefer the indexed decode so colour cycling can be animated. HAM
        // images (and any non-palette case) fall back to a fixed RGBA frame.
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
                    frame: vec![0u8; width * height * 4],
                    palette: img.palette,
                    indices: img.indices,
                    ranges,
                    start: Instant::now(),
                    last_offsets: Vec::new(),
                };
                // Render the initial (unrotated) frame so the first presented
                // frame is correct even before any time has elapsed.
                emu.render(0.0);
                emu.last_offsets = emu.cycle_offsets(0.0);
                Ok(emu)
            }
            Err(_) => {
                let img = ilbm::load(game)?;
                let (w, h) = img.dimensions();
                Ok(Self {
                    width: w as usize,
                    height: h as usize,
                    frame: img.into_raw(),
                    palette: Vec::new(),
                    indices: Vec::new(),
                    ranges: Vec::new(),
                    start: Instant::now(),
                    last_offsets: Vec::new(),
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
            let o = i * 4;
            self.frame[o] = c[0];
            self.frame[o + 1] = c[1];
            self.frame[o + 2] = c[2];
            self.frame[o + 3] = 255;
        }
    }
}

impl RetroEmu for ImageEmu {
    // Advance the colour-cycling animation. A static image (no active ranges)
    // leaves the frame untouched; otherwise the frame is only rebuilt when the
    // rotation has actually moved.
    fn run(&mut self) -> bool {
        let elapsed = self.start.elapsed().as_secs_f64();
        let offsets = self.cycle_offsets(elapsed);
        if offsets != self.last_offsets {
            self.render(elapsed);
            self.last_offsets = offsets;
        }
        true
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u8])) {
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
        60.0
    }

    fn save_png(&self, path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let buf =
            image::RgbaImage::from_raw(self.width as u32, self.height as u32, self.frame.clone())
                .ok_or("failed to build image buffer")?;
        buf.save(path)?;
        Ok(())
    }

    // Everything below is inapplicable to a still image.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&self) -> u32 {
        0
    }
    fn reset(&mut self) {}
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn unload(&mut self) {}
    fn skip_frames(&mut self, _frames: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_emu_presents_ilbm_frame() {
        let mut emu = ImageEmu::new(Path::new("testdata/test.iff")).unwrap();
        assert_eq!(emu.get_frame_size(), (640, 512));
        // `run` always succeeds; the frame is ready immediately.
        assert!(emu.run());
        emu.with_frame(&mut |w, h, frame| {
            assert_eq!((w, h), (640, 512));
            assert_eq!(frame.len(), w * h * 4);
            // The decoded image must not be a single flat color.
            let first = &frame[0..4];
            assert!(
                frame.chunks_exact(4).any(|px| px != first),
                "presented frame is blank"
            );
        });
    }

    #[test]
    fn color_cycling_changes_the_frame() {
        // Cycling is opt-in, so enable it via the tag `--color-cycle` sets.
        let mut emu = ImageEmu::new(Path::new("testdata/test.iff")).unwrap();
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
