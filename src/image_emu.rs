//! Static image backend that presents a decoded still image through the same
//! frontend plumbing as an emulator core.
//!
//! [`ImageEmu`] implements [`RetroEmu`] so an image file slots into the exact
//! same pipeline as a libretro core or the Flash backend: it hands the frontend
//! one RGBA frame via [`with_frame`](RetroEmu::with_frame) and reports its
//! geometry, while every interactive method (input, disks, audio, reset) is a
//! no-op. Currently it decodes the Amiga ILBM/IFF format (see [`crate::ilbm`]).

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::ilbm;
use crate::retro_emu::RetroEmu;

/// Presents a single decoded image as an unchanging "frame".
pub struct ImageEmu {
    width: usize,
    height: usize,
    /// Tightly packed RGBA8, alpha opaque.
    frame: Vec<u8>,
}

impl ImageEmu {
    pub fn new(game: &Path, _tags: HashMap<String, String>) -> Result<Self> {
        let img = ilbm::load(game)?;
        let (width, height) = img.dimensions();
        Ok(Self {
            width: width as usize,
            height: height as usize,
            frame: img.into_raw(),
        })
    }
}

impl RetroEmu for ImageEmu {
    // A static image has nothing to step; the frame is already ready.
    fn run(&mut self) -> bool {
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

    // A nominal rate keeps the frontend's frame pacing happy without spinning;
    // there is nothing to advance, so the value is otherwise irrelevant.
    fn fps(&self) -> f64 {
        60.0
    }

    fn save_png(&self, path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let buf = image::RgbaImage::from_raw(
            self.width as u32,
            self.height as u32,
            self.frame.clone(),
        )
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
        let mut emu = ImageEmu::new(Path::new("test.iff"), HashMap::new()).unwrap();
        assert_eq!(emu.get_frame_size(), (640, 512));
        // `run` is a no-op that always succeeds; the frame is ready immediately.
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
}
