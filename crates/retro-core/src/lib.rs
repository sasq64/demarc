//! The libretro layer, with nothing of the frontend in it: loading a core,
//! driving it, and handing back frames and audio through [`backend::Backend`].
//!
//! Deliberately dependency-light — no Bevy, no wgpu, no archive or network
//! crates — so it cross-compiles to targets the rest of demarc cannot reach.
//! See `docs/ANDROID.md`.

pub mod backend;
mod find;
#[allow(warnings)]
pub mod libretro;
mod path;
pub mod pixels;
pub mod retro_emu;

#[cfg(target_os = "android")]
pub mod android;

#[cfg(feature = "player")]
pub mod player;

pub use find::{dylib_name, find_core, set_system_dir, system_dir};
pub use path::strip_verbatim_prefix;
