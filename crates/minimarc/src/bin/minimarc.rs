//! The desktop `main`: `minimarc [csdb.txt]`.

use anyhow::Result;
use minimarc::player::App;
use winit::event_loop::EventLoop;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let db = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| "demodb/csdb.txt".into());
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App::new(db.into()))?;
    Ok(())
}
