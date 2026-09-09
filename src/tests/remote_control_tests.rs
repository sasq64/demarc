use super::*;

/// Write `source` to a uniquely named file so the tests can run in parallel,
/// and hand back a runner for it.
fn runner(name: &str, source: &str) -> Result<ScriptRunner> {
    let path = std::env::temp_dir().join(format!("remote_control_{name}.lua"));
    std::fs::write(&path, source).unwrap();
    ScriptRunner::new(&path)
}

/// One `update()` per frame, collecting what each frame produced.
fn frames(runner: &mut ScriptRunner, n: usize) -> Vec<Vec<Action>> {
    (0..n).map(|_| runner.update()).collect()
}

fn names(actions: &[Action]) -> Vec<String> {
    actions
        .iter()
        .map(|a| match a {
            Action::KeyDown(k) => format!("down {k:?}"),
            Action::KeyUp(k) => format!("up {k:?}"),
            Action::Cmd(c) => format!("cmd {c:?}"),
            Action::Screenshot(p) => format!("shot {}", p.display()),
            Action::Quit => "quit".into(),
        })
        .collect()
}

/// `wait_frames` must actually suspend: the command lands on the frame after
/// the wait, and every frame in between produces nothing.
#[test]
fn wait_frames_suspends_the_coroutine() {
    let mut r = runner(
        "wait",
        "function Main() wait_frames(3) send_cmd(Cmd.NextFile) end",
    )
    .unwrap();
    let f = frames(&mut r, 5);
    assert!(names(&f[0]).is_empty());
    assert!(names(&f[1]).is_empty());
    assert!(names(&f[2]).is_empty());
    assert_eq!(names(&f[3]), ["cmd NextFile"]);
    assert!(r.finished());
}

/// `press_key` is a down, a two-frame gap, then an up -- so a core that only
/// samples once per frame cannot miss it.
#[test]
fn press_key_holds_for_two_frames() {
    let mut r = runner("press", "function Main() press_key(Key.Enter) end").unwrap();
    let f = frames(&mut r, 4);
    assert_eq!(names(&f[0]), ["down Enter"]);
    assert!(names(&f[1]).is_empty());
    assert_eq!(names(&f[2]), ["up Enter"]);
}

/// Everything queued between two yields comes back in one batch, in order.
#[test]
fn actions_queue_in_order() {
    let mut r = runner(
        "order",
        r#"function Main()
            send_cmd(Cmd.Reset)
            screenshot("out.png")
            key_down(Key.KeyA)
            key_up(Key.KeyA)
            quit()
        end"#,
    )
    .unwrap();
    assert_eq!(
        names(&r.update()),
        ["cmd Reset", "shot out.png", "down KeyA", "up KeyA", "quit"]
    );
}

/// A name that is not a key must fail loudly rather than be dropped, and the
/// traceback must not take the process with it.
#[test]
fn an_unknown_key_is_an_error() {
    let mut r = runner(
        "badkey",
        r#"function Main() key_down("Nonsense") send_cmd(Cmd.Reset) end"#,
    )
    .unwrap();
    assert!(names(&r.update()).is_empty());
    assert!(r.finished());
}

/// A script that raises latches a traceback instead of panicking, and the
/// coroutine is done afterwards.
#[test]
fn a_failing_script_is_caught() {
    let mut r = runner("boom", "function Main() error('boom') end").unwrap();
    _ = r.update();
    assert!(r.finished());
    // Further updates are inert rather than resuming a dead coroutine.
    assert!(names(&r.update()).is_empty());
}

/// The entry point is `Main`; without it there is nothing to resume.
#[test]
fn a_script_without_main_fails_to_load() {
    assert!(runner("nomain", "X = 1").is_err());
}

/// `Key` and `Cmd` are tables of names, so a typo reaches Rust as nil rather
/// than as a plausible-looking string.
#[test]
fn the_name_tables_are_populated() {
    let mut r = runner(
        "tables",
        r#"function Main()
            send_cmd(Cmd.ShaderDialog)
            key_down(Key.ArrowUp)
            assert(Cmd.NoSuchCommand == nil)
            assert(Key.NoSuchKey == nil)
        end"#,
    )
    .unwrap();
    assert_eq!(names(&r.update()), ["cmd ShaderDialog", "down ArrowUp"]);
    assert!(r.finished());
}

/// Every `Cmd` round-trips through the name the Lua table exposes.
#[test]
fn every_command_round_trips_by_name() {
    for cmd in Cmd::ALL {
        assert_eq!(Cmd::from_name(&format!("{cmd:?}")), Some(*cmd));
    }
    assert_eq!(Cmd::from_name("NotACommand"), None);
}
