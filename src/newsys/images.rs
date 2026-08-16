use std::{fs, path::PathBuf};

use crate::{degas, image_emu::ImageEmu, newsys::walk_dir, retro_emu::Backend, workfile::WorkFile};
use anyhow::Result;

use super::System;

/// Formats that carry their own palette, and with it any colour cycling — an
/// ILBM or a DEGAS picture is the release itself, not a screenshot of one.
const INDEXED_EXTENSIONS: [&str; 9] = [
    "iff", "ilbm", "lbm", // Amiga
    "pi1", "pi2", "pi3", // Atari ST, DEGAS uncompressed (low/medium/high res)
    "pc1", "pc2", "pc3", // Atari ST, DEGAS compressed
];

/// Truecolour formats, which in a release directory are almost always a
/// screenshot of the real thing.
const SCREENSHOT_EXTENSIONS: [&str; 7] = ["png", "bmp", "jpg", "jpeg", "gif", "tif", "tiff"];

/// Enough of a file for both content checks below: the 34-byte DEGAS header,
/// which is the longer of the two.
const HEADER_LEN: usize = 34;

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
            println!("{path:?} {ext:?}");
            let len = fs::metadata(path)?.len() as usize;
            let is_ilbm =
                header.len() >= 12 && &header[0..4] == b"FORM" && &header[8..12] == b"ILBM";
            // Sniffed as well as matched by extension: DEGAS files turn up
            // named after the demo they came from as often as `.pi1`.
            if INDEXED_EXTENSIONS.contains(&ext) || is_ilbm || degas::is_degas(header, len) {
                images.push((0, path.to_owned()));
            } else if SCREENSHOT_EXTENSIONS.contains(&ext) {
                images.push((1, path.to_owned()));
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
        println!("PATH {path:?}");
        let backend = Box::new(ImageEmu::new(&path)?);
        Ok(backend)
    }
}
