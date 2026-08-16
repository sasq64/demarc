use std::collections::HashMap;

use super::System;
use super::utils::{build_m3u, sort_disks};
use crate::{Args, newsys::walk_dir, workfile::WorkFile};
use anyhow::Result;

const CORE_NAME_ATARIXL: &str = "atari800";

/// Bytes read per file, enough for the whole first header of a binary (see
/// [`is_atari_binary`]).
const HEADER_LEN: usize = 6;

/// An Atari 8-bit executable — the DOS binary load format, carrying whatever
/// extension (`.xex`, `.com`, `.exe`, `.bin`, none at all) the release felt
/// like, so it has to be recognized by content: `$FFFF` followed by the first
/// segment's start and end address, little endian, start not past end.
///
/// The core detects the format the same way and loads it without needing DOS,
/// so the file can be handed over as it is.
fn is_atari_binary(header: &[u8]) -> bool {
    if header.len() < HEADER_LEN || header[0..2] != [0xff, 0xff] {
        return false;
    }
    let start = u16::from_le_bytes([header[2], header[3]]);
    let end = u16::from_le_bytes([header[4], header[5]]);
    // `$FFFF` may repeat before a segment, but never as the address itself.
    start != 0xffff && start <= end
}

pub struct AtariXlSystem {}

impl AtariXlSystem {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for AtariXlSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_ATARIXL
    }

    fn name(&self) -> &'static str {
        "Atari XL"
    }
    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("atari800_ntscpal", "PAL"),
            ("atari800_system", "Modern XL/XE(1088K)"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut exes = vec![];
        walk_dir(&file.path.clone(), HEADER_LEN, |path, ext, header| {
            println!("{path:?} {ext:?}");
            if ["atr", "atx", "xfd", "dcm"].contains(&ext) {
                images.push(path.to_owned());
            } else if is_atari_binary(header) {
                exes.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            if images.len() > 1 {
                sort_disks(&mut images);
                let m3u = build_m3u(&images, file)?;
                file.path = m3u;
            } else {
                file.path = images[0].clone();
            }
        } else if !exes.is_empty() {
            // Nothing distinguishes the main program from any extra binary a
            // release carries, so take them in a stable order rather than
            // whatever order the directory walk happened to produce.
            exes.sort();
            file.path = exes[0].clone();
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
