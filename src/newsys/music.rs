//! Bare music files (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, AHX, TFMX, …),
//! played by [`MusicEmu`] rather than a libretro core.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

use crate::Args;
use crate::music_emu::{self, MusicEmu};
use crate::retro_emu::Backend;
use crate::system_dir;
use crate::workfile::WorkFile;

use super::System;

fn music_data_dir() -> PathBuf {
    system_dir().join("musix")
}

/// The Luau script that draws the picture for a song (see [`crate::music_vis`]).
///
/// `--lua` wins outright, and is taken as given rather than probed for: someone
/// who named a script on the command line wants to hear about a typo in it (as
/// a load error from the visualizer) rather than to silently get the default.
/// Failing that, a copy in the user's config directory wins, so a visualization
/// can be worked on without touching the installed files; otherwise the one
/// shipped in `system/` is used. `build.rs` packs that whole directory into the
/// embedded `system.zip`, so the default is always there — but debug builds
/// read the repo's `system/` in place, which is what makes editing it
/// worthwhile.
fn vis_script(from_args: Option<&Path>) -> Option<PathBuf> {
    if let Some(chosen) = from_args {
        return Some(chosen.to_path_buf());
    }
    if let Some(user) = dirs::config_dir().map(|d| d.join("demarc/scope.lua"))
        && user.is_file()
    {
        return Some(user);
    }
    let bundled = system_dir().join("lua/scope.lua");
    bundled.is_file().then_some(bundled)
}

pub struct MusicSystem {
    /// `--lua`, if it was given.
    lua: Option<PathBuf>,
}

impl MusicSystem {
    pub fn new(args: &Args) -> Self {
        Self {
            lua: args.lua.clone(),
        }
    }
}

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
            "smod", "dm2", "ahx", "aon", "mt2", "mon", "dw", "fred", "smod", "hip", "cus", "fc",
            "cm", "fp", "syn", "ma", "hipc", // Amiga
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
        Ok(Box::new(MusicEmu::new(
            path,
            &music_data_dir(),
            vis_script(self.lua.as_deref()).as_deref(),
        )?))
    }
}
