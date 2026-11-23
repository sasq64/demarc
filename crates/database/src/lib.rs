//! The releases demarc can play: demo databases, file and directory listings,
//! modland indexes and `overrides.toml`, read into [`emu_file::EmuFile`]s.

use release_io::{fetch, m3u, utils};

// The desktop one unpacks the bundled `system.zip` on first use.
#[cfg(feature = "desktop")]
use newsys::system_dir;
#[cfg(not(feature = "desktop"))]
use retro_core::system_dir;

pub mod csdb_latest;
pub mod demozoo_latest;
pub mod emu_file;
pub mod files;
pub mod load_error;
#[cfg(feature = "desktop")]
pub mod modland;
pub mod overrides;
pub mod playlists;
