//! The releases demarc can play: demo databases, file and directory listings,
//! modland indexes and `overrides.toml`, read into [`emu_file::EmuFile`]s.

use release_io::{fetch, m3u, utils};

#[cfg(feature = "desktop")]
use newsys::system_dir;

pub mod emu_file;
pub mod files;
pub mod load_error;
#[cfg(feature = "desktop")]
pub mod modland;
#[cfg(feature = "desktop")]
pub mod overrides;
