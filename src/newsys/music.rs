//! Bare music files (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, AHX, TFMX, …),
//! played by [`MusicEmu`] rather than a libretro core.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

use crate::music_emu::{self, MusicEmu};
use crate::retro_emu::Backend;
use crate::system_dir;
use crate::workfile::WorkFile;

use super::System;

fn music_data_dir() -> PathBuf {
    system_dir().join("musix")
}

pub struct MusicSystem {}

impl System for MusicSystem {
    // Only allow these extensions to avoid crashes
    fn extensions(&self) -> &'static [&'static str] {
        &[
            "sid", // C64
            "mod", "xm", "s3m", "it", // Trackers
            "snd", "sndh", "sap", // Atari
            "nsf", "gbs", "spc", "psf", // Console
            "mp3", "flac", // Streaming
            "vtx", "pt1", "pt2", "pt3", "asc", "sqt", "stc", "stp", "psc", // Spectrum
            "smod", "dm2", "ahx", "aon", "mt2", "mon", // Amiga
        ]
    }

    fn name(&self) -> &'static str {
        "Music"
    }

    fn can_load(&self, path: &Path) -> bool {
        let prefix = path
            .file_name()
            .and_then(|p| p.to_str())
            .and_then(|p| p.split('.').next())
            .map(|p| p.to_lowercase())
            .unwrap_or_default();
        let name_match = self.handles_ext(path)
            || (prefix == "mod" || prefix == "mdat" || prefix == "xm" || prefix == "stk");
        name_match && music_emu::can_handle(path, &music_data_dir())
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        info!("MUSIC CREATE {path:?}");
        Ok(Box::new(MusicEmu::new(path, &music_data_dir())?))
    }
}
