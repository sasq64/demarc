use std::{fs, path::PathBuf};

use crate::{
    backend::Backend, degas, image_emu::ImageEmu, newsys::walk_dir, workfile::WorkFile, zx_scr,
};
use anyhow::Result;

use super::System;

/// Formats that carry their own palette, and with it any colour cycling — an
/// ILBM or a DEGAS picture is the release itself, not a screenshot of one.
const INDEXED_EXTENSIONS: [&str; 16] = [
    "iff", "ilbm", "lbm", // Amiga
    "pi1", "pi2", "pi3", // Atari ST, DEGAS uncompressed (low/medium/high res)
    "pc1", "pc2", "pc3", // Atari ST, DEGAS compressed
    "neo", // Atari ST, NEOchrome (only ever low res)
    "ca1", "ca2", "ca3", // Atari ST, CrackArt
    "kid", // Atari ST, Fullscreen Construction Kit (overscanned, only low res)
    "pcx", // PC, the VGA era's paletted format and what gfx compos were entered in
    "scr", // ZX Spectrum, a raw dump of video RAM
];

/// Truecolour formats, which in a release directory are almost always a
/// screenshot of the real thing.
const SCREENSHOT_EXTENSIONS: [&str; 8] = ["png", "bmp", "jpg", "jpeg", "gif", "tif", "tiff", "tga"];

/// Enough of a file for both content checks below: what an ST picture's sniff
/// reads, which is the longer of the two.
const HEADER_LEN: usize = degas::SNIFF_BYTES;

pub struct ImageSystem {}

impl System for ImageSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &INDEXED_EXTENSIONS
    }
    fn name(&self) -> &'static str {
        "Images"
    }
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        // Rank first, path second: a picture that brings its own palette wins
        // over a screenshot sitting in the same directory.
        let mut images: Vec<(u8, PathBuf)> = vec![];
        walk_dir(&file.path.clone(), HEADER_LEN, |path, ext, header| {
            let len = fs::metadata(path)?.len() as usize;
            let is_ilbm =
                header.len() >= 12 && &header[0..4] == b"FORM" && &header[8..12] == b"ILBM";
            // Sniffed as well as matched by extension: an ST picture turns up
            // named after the demo it came from as often as `.pi1`.
            let indexed = match ext {
                // A Spectrum screen has no header at all, so its size is the
                // only check there is — and it is what keeps an unrelated
                // `.scr` (a script, a screensaver) out of the running.
                "scr" => zx_scr::is_screen(len),
                _ => {
                    INDEXED_EXTENSIONS.contains(&ext) || is_ilbm || degas::is_st_image(header, len)
                }
            };
            let n = path.components().count() as u8;
            if indexed {
                images.push((n, path.to_owned()));
            } else if ext == "jpg" || ext == "jpeg" {
                images.push((20 + n, path.to_owned()));
            } else if SCREENSHOT_EXTENSIONS.contains(&ext) {
                images.push((10 + n, path.to_owned()));
            }
            Ok(())
        })?;
        if images.is_empty() {
            return Ok(false);
        }

        // Stable, so files of equal rank stay in the order they were walked.
        images.sort_by_key(|(rank, _)| *rank);
        file.path = images.swap_remove(0).1;
        Ok(true)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let backend = Box::new(ImageEmu::new(path)?);
        Ok(backend)
    }
}
