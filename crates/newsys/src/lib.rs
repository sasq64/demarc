//! Everything between a file on disk and a `Box<dyn Backend>`: unpacking the
//! release, working out which machine it belongs to, fetching the core, and
//! building the backend that runs it.
//!
//! Frontend-free by construction — no Bevy, no window, no ECS — because the
//! frontend runs both halves of the pipeline off the main thread.

// The libretro layer, re-exported so this crate's modules keep their
// `crate::backend` / `crate::retro_emu` paths.
pub use retro_core::{backend, libretro, pixels, retro_emu};
// Likewise for the release plumbing, which lives where Android can build it.
pub use release_io::{c64, cache, cbmconvert, fetch, m3u, release_override, utils, workfile};

pub mod boxedapp;
pub mod com_dropper;
pub mod crinkler;
pub mod degas;
pub mod ffmpeg_emu;
pub mod health;
pub mod ilbm;
pub mod image_emu;
pub mod libloader;
pub mod music_emu;
pub mod music_vis;
pub mod gpu;
pub mod newsys;
pub mod opts;
pub mod system_dir;
pub mod text_renderer;
pub mod tiff_pal;
#[cfg(target_os = "windows")]
pub mod win_runner;
pub mod wine;
#[cfg(target_os = "linux")]
pub mod wine_log;
#[cfg(target_os = "linux")]
pub mod wine_sandbox;
pub mod x86;
pub mod zx_scr;

pub use newsys::*;
pub use opts::{CbmSystem, SysOpts};
pub use release_override::{Override, Patch, leak};
pub use system_dir::system_dir;
