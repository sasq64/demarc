//! The `c64` player's entry point. Everything it does lives in
//! [`retro_core::player`]; this is only the desktop `main`.

use anyhow::Result;
use retro_core::player::{App, load_core};
use winit::event_loop::EventLoop;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let core = load_core()?;
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App::new(core))?;
    Ok(())
}
