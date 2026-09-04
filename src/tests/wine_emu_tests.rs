use super::*;
use std::io::Write;
use std::os::fd::FromRawFd;

/// A pipe, as `(read end, write end)`.
fn pipe() -> (File, File) {
    let mut fds = [0; 2];
    // SAFETY: `pipe` writes two fds into an array of the right size.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "no pipe");
    unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
}

fn joins_within(handle: JoinHandle<()>, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() {
        if Instant::now() > deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    handle.join().is_ok()
}

fn drain_thread(read: File, stop: &Arc<AtomicBool>, signals: &Arc<Signals>) -> JoinHandle<()> {
    let stop = Arc::clone(stop);
    let signals = Arc::clone(signals);
    // Labelled as the driver's stream: that is the one the reports and the
    // signs of life come up.
    thread::spawn(move || drain(vec![(OwnedFd::from(read), DRIVER_STREAM)], &stop, &signals))
}

/// The reader has to keep up — gamescope and wine fill a pipe and then
/// block on it, taking the demo down with them — and it has to notice the
/// end when it comes.
#[test]
fn drains_a_pipe_and_returns_when_it_ends() {
    let (read, mut write) = pipe();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = drain_thread(read, &stop, &Arc::new(Signals::default()));

    // Several times a pipe buffer, so these writes can only complete
    // because something is emptying the other end.
    let filler = "x".repeat(1000);
    for i in 0..256 {
        writeln!(write, "line {i} {filler}").expect("the reader stopped reading");
    }
    // ...and a last line with no newline on it, which still gets logged.
    write!(write, "no newline here").unwrap();
    drop(write);

    assert!(
        joins_within(reader, Duration::from_secs(5)),
        "the reader did not notice the pipe ending"
    );
}

/// The bug this backend shipped with: wine's service processes put
/// themselves in their own sessions and inherit demarc's stdout and stderr,
/// so the pipes stay open after the demo's whole process group is killed.
/// A reader parked in `read` on one of those could not be joined, and the
/// quit hung in `Session::drop` for as long as it was left alone.
#[test]
fn a_pipe_someone_else_holds_open_cannot_wedge_the_quit() {
    // Kept alive to the end of the test, the way `winedevice.exe` keeps its
    // copy of the write end.
    let (read, _write) = pipe();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = drain_thread(read, &stop, &Arc::new(Signals::default()));

    thread::sleep(Duration::from_millis(300));
    assert!(
        !reader.is_finished(),
        "the reader gave up on a pipe that is still open"
    );

    stop.store(true, Ordering::Relaxed);
    assert!(
        joins_within(reader, Duration::from_secs(2)),
        "the reader would not stop - a quit here hangs demarc"
    );
}

/// Wait for `f`, or give up. The reports arrive on the logger thread.
fn becomes_true(mut f: impl FnMut() -> bool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while !f() {
        if Instant::now() > deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

/// The driver's reports are the only honest account of the demo there is —
/// gamescope outlives it, see `Signals` — so they have to survive the trip
/// through the log pipe: mixed in with everything wine has to say, and read
/// as news rather than logged and forgotten.
#[test]
fn the_drivers_reports_come_back_through_the_log() {
    let (read, mut write) = pipe();
    let stop = Arc::new(AtomicBool::new(false));
    let signals = Arc::new(Signals::default());
    let reader = drain_thread(read, &stop, &signals);

    writeln!(write, "fixme:win:something wine has to say").unwrap();
    // The driver is a PE writing to a pipe, so its lines arrive with the
    // carriage return still on them.
    write!(write, "{SENTINEL}started\r\n").unwrap();
    assert!(
        becomes_true(
            || signals.started.load(Ordering::Relaxed),
            Duration::from_secs(2)
        ),
        "the start of the demo was not heard"
    );
    assert!(
        !signals.ended.load(Ordering::Relaxed),
        "a running demo was taken for a finished one"
    );

    writeln!(write, "undecorated the demo window (800x600)").unwrap();
    writeln!(write, "{SENTINEL}exited").unwrap();
    assert!(
        becomes_true(
            || signals.ended.load(Ordering::Relaxed),
            Duration::from_secs(2)
        ),
        "the end of the demo was not heard - the session would hold the screen"
    );

    drop(write);
    assert!(joins_within(reader, Duration::from_secs(5)));
}

/// A demo that never starts has to end the session too, or demarc sits
/// behind an empty gamescope forever.
#[test]
fn a_demo_that_never_started_ends_the_session() {
    let (read, mut write) = pipe();
    let stop = Arc::new(AtomicBool::new(false));
    let signals = Arc::new(Signals::default());
    let reader = drain_thread(read, &stop, &signals);

    writeln!(write, "{SENTINEL}failed").unwrap();
    assert!(
        becomes_true(
            || signals.ended.load(Ordering::Relaxed),
            Duration::from_secs(2)
        ),
        "a demo that could not be started was left running"
    );
    assert!(!signals.started.load(Ordering::Relaxed));

    drop(write);
    assert!(joins_within(reader, Duration::from_secs(5)));
}

/// A driver too old to report anything must not have its demo shot at
/// `START_TIMEOUT`: it still says what it is doing, and that is enough to
/// know something is running in there.
#[test]
fn an_older_drivers_chatter_still_counts_as_a_sign_of_life() {
    let (read, mut write) = pipe();
    let stop = Arc::new(AtomicBool::new(false));
    let signals = Arc::new(Signals::default());
    let reader = drain_thread(read, &stop, &signals);

    writeln!(write, "launched /demos/x.exe as pid 42").unwrap();
    assert!(
        becomes_true(
            || signals.heard.load(Ordering::Relaxed),
            Duration::from_secs(2)
        ),
        "a driver that is plainly running was not heard"
    );
    assert!(!signals.started.load(Ordering::Relaxed));
    assert!(!signals.ended.load(Ordering::Relaxed));

    drop(write);
    assert!(joins_within(reader, Duration::from_secs(5)));
}

/// The pile-up this exists to stop: a process left in the prefix that
/// `wineserver -k` cannot reach, because the server it belonged to has
/// already gone. Stood in for here by a `sleep` carrying the same
/// `WINEPREFIX`, which is all the sweep matches on.
#[test]
fn a_process_left_in_the_prefix_is_swept_up() {
    let prefix = std::env::temp_dir().join(format!("demarc-sweep-{}", std::process::id()));
    let mut leftover = Command::new("sleep")
        .arg("60")
        .env("WINEPREFIX", &prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sleep");
    // A neighbour in a prefix of its own, to be left alone.
    let mut other = Command::new("sleep")
        .arg("60")
        .env("WINEPREFIX", prefix.join("elsewhere"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sleep");

    sweep_prefix(&prefix, None);
    assert!(
        becomes_true(
            || matches!(leftover.try_wait(), Ok(Some(_))),
            Duration::from_secs(5)
        ),
        "the leftover survived the sweep"
    );
    assert!(
        matches!(other.try_wait(), Ok(None)),
        "the sweep took something out of another prefix"
    );

    let _ = other.kill();
    let _ = other.wait();
}

/// And the pid it is told to spare — the `wineserver -k` still working
/// through the prefix — has to survive it.
#[test]
fn the_sweep_spares_the_pid_it_is_given() {
    let prefix = std::env::temp_dir().join(format!("demarc-spare-{}", std::process::id()));
    let mut spared = Command::new("sleep")
        .arg("60")
        .env("WINEPREFIX", &prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("sleep");

    sweep_prefix(&prefix, Some(spared.id()));
    thread::sleep(Duration::from_millis(200));
    assert!(
        matches!(spared.try_wait(), Ok(None)),
        "the sweep killed the process it was told to spare"
    );

    let _ = spared.kill();
    let _ = spared.wait();
}

#[test]
fn reads_a_resolution_or_falls_back_to_the_default() {
    assert_eq!(parse_res("800x600"), Some((800, 600)));
    assert_eq!(parse_res(" 1280 X 720 "), Some((1280, 720)));
    assert_eq!(parse_res("640"), None);
    assert_eq!(parse_res("wide x tall"), None);
    assert_eq!(parse_res(DEFAULT_RES), Some((800, 600)));
}

/// The config has to survive whatever the metadata says, since it comes
/// from a database line or the command line and neither is checked.
#[test]
fn a_broken_resolution_still_gives_a_usable_config() {
    let exe = std::env::current_exe().expect("this test binary");
    let meta = HashMap::from([(META_RES.to_string(), "huge".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (800, 600));

    let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (1024, 768));

    // An empty value is nothing said, not a broken resolution.
    let meta = HashMap::from([(META_RES.to_string(), String::new())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (800, 600));
    assert_eq!(cfg.dialog, Dialog::Drive);
}

/// `wine_res=pick` hands the dialog to whoever is watching: no driver in
/// the command at all, and a session big enough for whatever they choose.
#[test]
fn pick_leaves_the_dialog_alone() {
    let exe = std::env::current_exe().expect("this test binary");
    for spelling in ["pick", "PICK", "  Pick  "] {
        let meta = HashMap::from([(META_RES.to_string(), spelling.to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!(cfg.dialog, Dialog::Pick, "{spelling:?}");
        assert_eq!((cfg.width, cfg.height), (1920, 1200), "{spelling:?}");

        // The driver is still the command - it is what starts the demo
        // and what reports its end - but it is told to press nothing and
        // to leave the demo's window alone.
        let args = cfg.wine_args(Some(Path::new("/sys/win/autodlg.exe")));
        assert_eq!(args[0], "/sys/win/autodlg.exe", "{spelling:?}");
        assert!(args.contains(&"--no-go".to_string()), "{spelling:?}");
        assert!(args.contains(&"--no-fill".to_string()), "{spelling:?}");
        // Nothing chosen and nothing ticked: those are the dialog's own.
        assert!(!args.contains(&"--prefer".to_string()), "{spelling:?}");
        assert!(!args.contains(&"--check".to_string()), "{spelling:?}");
        let launch = args.iter().position(|a| a == "--launch").expect("--launch");
        assert_eq!(args[launch + 1], cfg.exe.to_string_lossy(), "{spelling:?}");
    }

    // Any other value is still a resolution, and still driven.
    let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!(cfg.dialog, Dialog::Drive);
    let args = cfg.wine_args(Some(Path::new("/a.exe")));
    assert!(args.contains(&"--prefer".to_string()));
    assert!(!args.contains(&"--no-go".to_string()));
}

/// The demo has to be started *by* the driver, in the same session, or the
/// driver's `EnumWindows` never sees the dialog it exists to answer.
#[test]
fn the_driver_launches_the_demo() {
    let exe = std::env::current_exe().expect("this test binary");
    let cfg = Config::from_meta(&exe, &HashMap::new()).unwrap();
    let args = cfg.wine_args(Some(Path::new("/sys/win/autodlg.exe")));

    // No virtual desktop unless an entry asks for one: wine runs the
    // driver directly.
    assert!(!cfg.desktop);
    assert_eq!(args[0], "/sys/win/autodlg.exe");
    let launch = args.iter().position(|a| a == "--launch").expect("--launch");
    assert_eq!(args[launch + 1], cfg.exe.to_string_lossy());
    // The size demarc runs at is the size the dialog gets told to pick.
    let prefer = args.iter().position(|a| a == "--prefer").expect("--prefer");
    assert_eq!(args[prefer + 1], "800x600");

    // Without a driver the demo is the one command.
    let bare = cfg.wine_args(None);
    assert_eq!(bare, vec![cfg.exe.to_string_lossy().into_owned()]);
}

/// `wine_desktop=true` wraps whatever would have run in a wine virtual
/// desktop the size of the session. A handful of demos - Equinox's *Kings
/// of the Playground* among them - do not survive a real display mode
/// change under gamescope's Xwayland, and this is what saves them.
#[test]
fn a_virtual_desktop_wraps_the_command_when_asked_for() {
    let exe = std::env::current_exe().expect("this test binary");
    let driver = Path::new("/sys/win/autodlg.exe");

    for spelling in ["true", "1", "YES", " on "] {
        let meta = HashMap::from([(META_DESKTOP.to_string(), spelling.to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert!(cfg.desktop, "{spelling:?}");

        let args = cfg.wine_args(Some(driver));
        assert_eq!(args[0], "explorer", "{spelling:?}");
        assert_eq!(args[1], "/desktop=demarc,800x600", "{spelling:?}");
        // Everything the desktop hosts is still one command: the driver,
        // which starts the demo itself.
        assert_eq!(args[2], "/sys/win/autodlg.exe", "{spelling:?}");
    }

    // The desktop is the size of the session, whatever that turned out to
    // be - including a `pick` session, which is why it is big.
    let meta = HashMap::from([
        (META_DESKTOP.to_string(), "true".to_string()),
        (META_RES.to_string(), PICK.to_string()),
    ]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    let args = cfg.wine_args(Some(driver));
    assert_eq!(args[1], "/desktop=demarc,1920x1200");
    // ...and it hosts the driver even with the dialog left alone, since
    // the driver is what starts the demo inside it.
    assert_eq!(args[2], "/sys/win/autodlg.exe");
    assert!(args.contains(&"--no-go".to_string()));

    // Anything else is a no, including nonsense and an empty value.
    for spelling in ["false", "no", "0", "", "maybe"] {
        let meta = HashMap::from([(META_DESKTOP.to_string(), spelling.to_string())]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert!(!cfg.desktop, "{spelling:?}");
        assert_ne!(cfg.wine_args(Some(driver))[0], "explorer", "{spelling:?}");
    }
}
