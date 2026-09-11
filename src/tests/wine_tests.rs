use super::*;

/// Wait for `f`, or give up.
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
    assert_eq!(parse_res(DEFAULT_RES), Some((1280, 1024)));
}

/// The config has to survive whatever the metadata says, since it comes
/// from a database line or the command line and neither is checked.
#[test]
fn a_broken_resolution_still_gives_a_usable_config() {
    let exe = std::env::current_exe().expect("this test binary");
    let meta = HashMap::from([(META_RES.to_string(), "huge".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (1280, 1024));

    let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (1024, 768));

    // An empty value is nothing said, not a broken resolution.
    let meta = HashMap::from([(META_RES.to_string(), String::new())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!((cfg.width, cfg.height), (1280, 1024));
    assert_eq!(cfg.dialog, Dialog::Drive("1280x1024".to_string()));
}

/// `wine_res` is the size of the session and nothing else; what the dialog is
/// asked for is `wine_dialog_res`, a list tried best first, and only when
/// nothing says otherwise is that the session's own size.
#[test]
fn the_dialog_is_asked_for_its_own_list_of_modes() {
    let exe = std::env::current_exe().expect("this test binary");
    let driver = Path::new("/sys/win/autodlg.exe");

    let meta = HashMap::from([
        (META_RES.to_string(), "1280x1024".to_string()),
        (META_DIALOG_RES.to_string(), " 640x480 , 800x600 ".to_string()),
    ]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    // The list does not touch the session, which stays what `wine_res` said.
    assert_eq!((cfg.width, cfg.height), (1280, 1024));
    let args = cfg.wine_args(Some(driver));
    let prefer = args.iter().position(|a| a == "--prefer").expect("--prefer");
    assert_eq!(args[prefer + 1], "640x480,800x600");

    // An empty list is nothing said: the session's size is what is asked for.
    let meta = HashMap::from([
        (META_RES.to_string(), "640x480".to_string()),
        (META_DIALOG_RES.to_string(), String::new()),
    ]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!(cfg.dialog, Dialog::Drive("640x480".to_string()));
}

/// `wine_dialog_res=pick` hands the dialog to whoever is watching: nothing
/// pressed at all, and the session keeps the size `wine_res` asked for.
#[test]
fn pick_leaves_the_dialog_alone() {
    let exe = std::env::current_exe().expect("this test binary");
    for spelling in ["pick", "PICK", "  Pick  "] {
        let meta = HashMap::from([
            (META_RES.to_string(), "1024x768".to_string()),
            (META_DIALOG_RES.to_string(), spelling.to_string()),
        ]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert_eq!(cfg.dialog, Dialog::Pick, "{spelling:?}");
        assert_eq!((cfg.width, cfg.height), (1024, 768), "{spelling:?}");

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

    // `pick` is not a size either, so `wine_res` still says what it always did.
    let meta = HashMap::from([(META_RES.to_string(), "1024x768".to_string())]);
    let cfg = Config::from_meta(&exe, &meta).unwrap();
    assert_eq!(cfg.dialog, Dialog::Drive("1024x768".to_string()));
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
    assert_eq!(args[prefer + 1], DEFAULT_RES);

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
        let meta = HashMap::from([
            (META_DESKTOP.to_string(), spelling.to_string()),
            (META_RES.to_string(), "800x600".to_string()),
        ]);
        let cfg = Config::from_meta(&exe, &meta).unwrap();
        assert!(cfg.desktop, "{spelling:?}");

        let args = cfg.wine_args(Some(driver));
        assert_eq!(args[0], "explorer", "{spelling:?}");
        assert_eq!(args[1], "/desktop=demarc,800x600", "{spelling:?}");
        // Everything the desktop hosts is still one command: the driver,
        // which starts the demo itself.
        assert_eq!(args[2], "/sys/win/autodlg.exe", "{spelling:?}");
    }

    // The desktop is the size of the session whatever is done with the dialog.
    let meta = HashMap::from([
        (META_DESKTOP.to_string(), "true".to_string()),
        (META_RES.to_string(), "1920x1200".to_string()),
        (META_DIALOG_RES.to_string(), PICK.to_string()),
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

/// `wine_dll_overrides` is wine's variable and goes to wine as it stands —
/// there is no second syntax to learn. What it is not is a variable that has to
/// be there: an empty one says nothing and is better left unset.
#[test]
fn carries_dll_overrides_through_untouched() {
    let of = |value: &str| {
        let meta = HashMap::from([(META_DLL_OVERRIDES.to_string(), value.to_string())]);
        dll_overrides(&meta)
    };

    assert_eq!(
        of("d3dx9_37,d3dx9_43=n;d3d9=n,b").as_deref(),
        Some("d3dx9_37,d3dx9_43=n;d3d9=n,b")
    );
    // Whitespace around it is a `-x` or an `overrides.toml` line, not part of
    // what wine is being told.
    assert_eq!(of("  d3dx9_37=n \n").as_deref(), Some("d3dx9_37=n"));

    assert_eq!(of(""), None);
    assert_eq!(of("   "), None);
    assert_eq!(dll_overrides(&HashMap::new()), None);
}

/// `wine_gl_compat` is a yes/no, spelled any of the ways the rest of demarc's
/// yes/nos are, and it is off unless an entry says otherwise — a compatibility
/// profile is for the demo that resolves GL entry points without checking them,
/// not for everything.
#[test]
fn reads_gl_compat_as_a_yes_or_no() {
    let of = |value: &str| {
        let meta = HashMap::from([(META_GL_COMPAT.to_string(), value.to_string())]);
        gl_compat(&meta)
    };

    for spelling in ["true", "1", "YES", " on "] {
        assert!(of(spelling), "{spelling:?}");
    }
    // Anything else is a no, including nonsense and an empty value.
    for spelling in ["false", "no", "0", "", "maybe"] {
        assert!(!of(spelling), "{spelling:?}");
    }

    assert_eq!(gl_compat(&HashMap::new()), DEFAULT_GL_COMPAT);
}

/// A `PATH` directory holds more than programs, and one of the extras
/// answering to the name of a tool we are about to run is worse than nothing —
/// the check would pass and the session would then fail to start. So the
/// executable bit is part of what "on PATH" means here.
#[test]
fn a_tool_on_the_path_has_to_be_executable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("demarc-find-{}", std::process::id()));
    let empty = dir.join("empty");
    std::fs::create_dir_all(&empty).expect("temp dir");
    let path = std::env::join_paths([&empty, &dir]).expect("search path");

    let backup = dir.join("wine.bak");
    std::fs::write(&backup, "not a program").expect("write");
    std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    assert_eq!(find_in(&path, "wine.bak"), None, "a plain file is not a tool");

    let tool = dir.join("wine");
    std::fs::write(&tool, "#!/bin/sh\n").expect("write");
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    assert_eq!(find_in(&path, "wine"), Some(tool));

    // And a directory of that name, which every `is_file` check exists to skip.
    assert_eq!(find_in(&path, "empty"), None);
    assert_eq!(find_in(&path, "nothing-of-the-sort"), None);

    let _ = std::fs::remove_dir_all(&dir);
}

/// The check looks for three things, and says so in the order it looked. What
/// it must never do is call a machine ready when one of them is missing:
/// `--check-wine` prints from this, and [`crate::newsys`] drops the Windows
/// system on the strength of it.
#[test]
fn a_check_is_ready_only_when_nothing_is_missing() {
    let names: Vec<_> = check_wine().needs.iter().map(|need| need.what).collect();
    assert_eq!(names, ["wine", "bwrap", "prefix"]);

    let need = |what, found: Result<&str, &str>| Need {
        what,
        found: found.map(PathBuf::from).map_err(str::to_string),
    };
    let all_there = WineCheck {
        needs: vec![
            need("wine", Ok("/usr/bin/wine")),
            need("bwrap", Ok("/usr/bin/bwrap")),
        ],
    };
    assert!(all_there.ok());
    assert_eq!(all_there.missing(), "");
    assert!(all_there.report().contains("can be run"));

    let short = WineCheck {
        needs: vec![
            need("wine", Ok("/usr/bin/wine")),
            need("bwrap", Err("not on PATH")),
            need("prefix", Err("is not there")),
        ],
    };
    assert!(!short.ok());
    assert_eq!(short.missing(), "bwrap, prefix");
    let report = short.report();
    assert!(report.contains("disabled"), "{report}");
    // Every requirement is in the report whether it was met or not: the point
    // of printing it is to see which one to go and fix.
    for what in ["wine", "bwrap", "prefix"] {
        assert!(report.contains(what), "{what} missing from {report}");
    }
}
