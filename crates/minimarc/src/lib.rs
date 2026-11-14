//! `minimarc` — demarc cut down to one thing: browsing the csdb database and
//! playing what is picked in VICE. winit, wgpu, egui and cpal, no Bevy, which
//! is the stack the Android build needs — see `docs/ANDROID.md`.

mod load;
pub mod player;
mod ui;
