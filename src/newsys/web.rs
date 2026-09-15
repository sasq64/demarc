use std::collections::HashMap;

use anyhow::Result;

use super::System;
use crate::backend::Backend;
#[cfg(target_os = "linux")]
use crate::libloader;
#[cfg(target_os = "linux")]
use crate::retro_emu::RetroCoreThreaded;
#[cfg(target_os = "linux")]
use crate::system_dir;
use crate::workfile::WorkFile;
#[cfg(target_os = "linux")]
use anyhow::Context;

/// HTML/JS releases, shown in an undecorated Chrome inside a gamescope session.
///
/// A web demo is a picture source like any other here: the gamescope core
/// composites the session headlessly and hands the frames back, so the page gets
/// the shaders, the grid and the screenshots that every other system gets. See
/// [`crate::newsys::windows`], which reaches the same core from the wine side,
/// and `docs/GAMESCOPE.md`.
///
/// Only the page is claimed, not the files around it. A release ships its
/// `.js`, its textures and its shaders beside the `.html`, and Chrome fetches
/// those itself over `file://` — they are not separately loadable and must not
/// be taken away from the image and music systems, which can at least show what
/// a release shipped.
pub struct WebSystem {}

/// gamescope and Chrome are only wired up on Linux, the same limitation the
/// wine side has — see [`super::windows`].
const CAN_RUN_WEB: bool = cfg!(target_os = "linux");

/// The core that runs the session. Not on the libretro buildbot, so it only
/// ever resolves through `DEMARC_CORE_DIR` — see [`crate::libloader`].
#[cfg(target_os = "linux")]
const CORE_NAME_GAMESCOPE: &str = "gamescope";

impl System for WebSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["html", "htm"]
    }

    fn can_load(&self, path: &std::path::Path) -> bool {
        CAN_RUN_WEB && self.handles_ext(path)
    }

    /// A page is 800x600 by default like everything else here, but unlike a
    /// demo it has no opinion of its own: nothing in an `.html` announces the
    /// size it wants, so the entry has to say.
    fn default_meta(&self) -> HashMap<&str, &str> {
        let mut meta: HashMap<&str, &str> = HashMap::new();
        meta.insert("gamescope_command", "chrome");
        meta.insert("gamescope_resolution", "800x600");
        meta
    }

    fn name(&self) -> &'static str {
        "Web"
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        #[cfg(target_os = "linux")]
        {
            let core = libloader::get_libretro(CORE_NAME_GAMESCOPE)
                .context("Could not load the gamescope core")?;
            Ok(Box::new(RetroCoreThreaded::new(
                &core,
                system_dir(),
                Some(path),
                path.get_all_meta(),
                false,
            )?))
        }
        // `can_load` said no everywhere else, so this is only reachable by
        // asking for a page by hand.
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!(
            "{:?} needs gamescope and Chrome, which demarc only has on Linux",
            path.path
        );
    }
}

#[cfg(test)]
#[path = "tests/web_tests.rs"]
mod tests;
