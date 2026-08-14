//! Bare music files (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, AHX, TFMX, …),
//! played by [`MusicEmu`] rather than a libretro core.

use std::path::{Path, PathBuf};

use anyhow::Result;

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
    fn extensions(&self) -> &'static [&'static str] {
        &[
            "sid", "mod", "xm", "s3m", "it", "sndh", "sap", "nsf", "gbs", "spc", "psf", "ahx",
            "mp3", "flac", "ogg",
        ]
    }

    fn name(&self) -> &'static str {
        "Music"
    }

    fn can_load(&self, path: &Path) -> bool {
        music_emu::can_handle(path, &music_data_dir())
    }

    /// A directory is left as it is rather than resolved down to a single song:
    /// [`MusicEmu`] picks the file to play out of it, by the same rule
    /// [`music_emu::can_handle`] used to say yes here.
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        Ok(self.can_load(file))
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        Ok(Box::new(MusicEmu::new(path, &music_data_dir())?))
    }
}
