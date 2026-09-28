//! The libretro layer, with nothing of the frontend in it: loading a core,
//! driving it, and handing back frames and audio through [`backend::Backend`].
//!
//! Deliberately dependency-light — no Bevy, no wgpu, no archive or network
//! crates — so it cross-compiles to targets the rest of demarc cannot reach.
//! See `docs/ANDROID.md`.

pub mod backend;
#[allow(warnings)]
pub mod libretro;
mod find;
mod path;
pub mod pixels;
pub mod retro_emu;

#[cfg(feature = "player")]
pub mod player;

pub use find::{dylib_name, find_core, system_dir};
pub use path::strip_verbatim_prefix;
