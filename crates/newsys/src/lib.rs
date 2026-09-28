//! Everything between a file on disk and a `Box<dyn Backend>`: unpacking the
//! release, working out which machine it belongs to, fetching the core, and
//! building the backend that runs it.
//!
//! Frontend-free by construction — no Bevy, no window, no ECS — because the
//! frontend runs both halves of the pipeline off the main thread.

// The libretro layer, re-exported so this crate's modules keep their
// `crate::backend` / `crate::retro_emu` paths.
pub use retro_core::{backend, libretro, pixels, retro_emu};

pub mod cache;
pub mod cbmconvert;
pub mod degas;
pub mod fetch;
pub mod ilbm;
pub mod image_emu;
pub mod libloader;
pub mod m3u;
pub mod music_emu;
pub mod music_vis;
pub mod newsys;
pub mod opts;
pub mod release_override;
pub mod system_dir;
pub mod tiff_pal;
pub mod utils;
#[cfg(target_os = "windows")]
pub mod win_runner;
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod wine;
#[cfg(target_os = "linux")]
pub mod wine_sandbox;
pub mod workfile;
pub mod zx_scr;

pub use newsys::*;
pub use opts::{CbmSystem, SysOpts};
pub use release_override::{Override, Patch, leak};
pub use system_dir::system_dir;
