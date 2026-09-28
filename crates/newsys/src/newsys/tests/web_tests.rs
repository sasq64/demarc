use super::*;
use std::path::Path;

/// A page is claimed; the files a page pulls in are not. Those belong to
/// whatever else can make something of them.
#[test]
fn claims_pages_and_nothing_around_them() {
    let sys = WebSystem {};
    let can = |name: &str| sys.can_load(Path::new(name));

    assert_eq!(can("intro.html"), CAN_RUN_WEB);
    assert_eq!(can("index.htm"), CAN_RUN_WEB);

    // Everything a release ships beside the page.
    assert!(!can("demo.js"));
    assert!(!can("shader.glsl"));
    assert!(!can("texture.png"));
    assert!(!can("tune.mod"));
    assert!(!can("readme.txt"));
}

/// The core is told what to run through its own options, so a page needs no
/// per-entry setup to work.
#[test]
fn asks_the_core_for_chrome() {
    let meta = WebSystem {}.default_meta();
    assert_eq!(meta.get("gamescope_command"), Some(&"chrome"));
    assert_eq!(meta.get("gamescope_resolution"), Some(&"800x600"));
}
