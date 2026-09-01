use std::collections::HashMap;

use anyhow::Result;

use super::System;
use crate::config::CbmSystem;
use crate::newsys::{collect_disk_images, walk_dir};
use crate::{Args, workfile::WorkFile};

/// yapesdl, built as a libretro core out of `external/yapesdl` — see its
/// `Makefile.libretro`. VICE's plus/4 emulation is incomplete enough that
/// releases ship warning about it, which is what this core is here for.
const CORE_NAME_YAPE: &str = "yape";

/// The Commodore 264 series: C16, C116 and plus/4.
///
/// Nothing in a `.prg` or a `.d64` says which Commodore it was written for —
/// a plus/4 program and a C64 one are the same shape — so the machine cannot
/// be detected and has to be asked for with `--cbm-variant c16`. Without it
/// this system stands aside and [`super::c64::C64System`] takes the release.
pub struct Plus4System {
    /// Whether `--cbm-variant` named this machine.
    selected: bool,
}

impl Plus4System {
    pub fn new(args: &Args) -> Self {
        Self {
            selected: matches!(args.cbm_variant, CbmSystem::C16),
        }
    }
}

impl System for Plus4System {
    fn core_name(&self) -> &'static str {
        CORE_NAME_YAPE
    }

    fn name(&self) -> &'static str {
        "C16/Plus4"
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        // The cycle-exact TED, which is the whole reason for preferring this
        // core; the "fast" model exists for machines that cannot keep up.
        [("yape_model", "Commodore Plus/4 (accurate)")].into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if file.get_meta_or("platform", "") != "C16" && !self.selected {
            return Ok(false);
        }

        let mut images = vec![];
        let mut programs = vec![];
        let mut tapes = vec![];

        walk_dir(file, 0, |path, ext, _header| {
            match ext {
                "d64" | "g64" => images.push(path.to_owned()),
                // The core loads all three straight into memory, so none of
                // them needs converting first the way the C64 side does.
                "prg" | "p00" | "t64" => programs.push(path.to_owned()),
                "tap" | "wav" => tapes.push(path.to_owned()),
                _ => {}
            }
            Ok(())
        })?;

        if !images.is_empty() {
            // A release with several disks becomes an m3u, which the core
            // reads as its disk list so sides can be swapped while it runs.
            collect_disk_images(file, &mut images)?;
        } else if !programs.is_empty() {
            // Nothing marks the main program among a release's extras, so
            // take them in a stable order rather than the walk's order.
            programs.sort();
            file.path = programs[0].clone();
        } else if !tapes.is_empty() {
            tapes.sort();
            file.path = tapes[0].clone();
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
