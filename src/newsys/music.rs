//! Bare music files (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, AHX, TFMX, …),
//! played by [`MusicEmu`] rather than a libretro core.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crate::backend::Backend;
use crate::music_emu::{self, MusicEmu};
use crate::system_dir;
use crate::utils::get_ext;
use crate::workfile::WorkFile;
use crate::{Args, libloader, retro_emu};

use super::System;

/// Meta key sending modules to the tracker that made them — ProTracker 2 or
/// Fasttracker II, screen, scopes and all — instead of to [`MusicEmu`].
pub const USE_TRACKER: &str = "use_tracker";

/// The cores built out of the `libretro/` directories in the pt2-clone and
/// ft2-clone forks, which are the trackers with their SDL2 replaced by
/// libretro. Not on the buildbot; see `libloader::ALT_SOURCES`.
const PROTRACKER_CORE: &str = "pt2clone";
const FASTTRACKER_CORE: &str = "ft2clone";

/// Whether `path` has one of `extensions`, named either way round.
fn is_named(path: &Path, extensions: &[&str]) -> bool {
    let name = path
        .file_name()
        .and_then(|p| p.to_str())
        .unwrap_or_default()
        .to_lowercase();
    extensions.contains(&get_ext(path).as_str())
        || extensions
            .iter()
            .any(|ext| name.starts_with(&format!("{ext}.")))
}

/// The tracker core that plays `path`: a 31- or 15-sample module goes to the
/// ProTracker clone, and what else the Fasttracker II clone loads goes there.
fn tracker_core(path: &Path) -> Option<&'static str> {
    if is_named(path, &["mod", "stk", "nst", "m15"]) {
        Some(PROTRACKER_CORE)
    } else if is_named(path, &["xm", "ft", "s3m", "stm", "fst", "digi", "bem"]) {
        Some(FASTTRACKER_CORE)
    } else {
        None
    }
}

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
            "mod", "xm", "s3m", "ft", "stm", "it", // Trackers
            "snd", "sndh", "sap", // Atari
            "nsf", "gbs", "spc", "psf", // Console
            "mp3", "flac", // Streaming
            "v2m",  //
            "emul", "vtx", "pt1", "pt2", "pt3", "asc", "sqt", "stc", "stp", "psc", // Spectrum
            "smod", "dm2", "ahx", "aon", "mt2", "mon", "dw", "fred", "smod", "hip", "cus", "fc",
            "hvl", "cm", "fp", "syn", "ma", "hipc", "ml", "mk2", "bd", "dln", "669", // Amiga
            "jam", "dbm", "bp", "bp3", "hes", "lds",
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
        if path.is_enabled(USE_TRACKER)
            && let Some(name) = tracker_core(path)
        {
            let core = libloader::get_libretro(name)
                .with_context(|| format!("Could not load the {name} core"))?;
            return retro_emu::create_core(
                &core,
                system_dir(),
                Some(path),
                path.get_all_meta(),
                false,
            );
        }
        Ok(Box::new(MusicEmu::new(
            path,
            &music_data_dir(),
            vis_script(self.lua.as_deref()).as_deref(),
        )?))
    }
}

#[cfg(test)]
#[path = "tests/music_tests.rs"]
mod tests;
