//! The releases demarc can play: demo databases, file and directory listings,
//! modland indexes and `overrides.toml`, read into [`emu_file::EmuFile`]s.

use newsys::{fetch, m3u, system_dir, utils};

pub mod emu_file;
pub mod files;
pub mod load_error;
pub mod modland;
pub mod overrides;
