//! The desktop and iOS `main`: `minimarc [csdb.txt]`.

use anyhow::Result;
use minimarc::player::App;
use winit::event_loop::EventLoop;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    #[cfg(target_os = "ios")]
    let dbs = minimarc::player::ios_setup()?;
    #[cfg(not(target_os = "ios"))]
    let dbs = {
        let args: Vec<_> = std::env::args_os().skip(1).map(Into::into).collect();
        if args.is_empty() {
            vec!["demodb/csdb.txt".into(), "demodb/demozoo-amiga.txt".into()]
        } else {
            args
        }
    };
    let event_loop = EventLoop::new()?;
    event_loop.run_app(&mut App::new(dbs))?;
    Ok(())
}
