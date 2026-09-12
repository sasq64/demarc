use super::*;

/// The arguments as words, so a test can ask about pairs without counting.
fn args(base: &str, prefix: &str, workdir: Option<&str>) -> Vec<String> {
    bwrap_args(Path::new(base), Path::new(prefix), workdir.map(Path::new))
}

/// Is `value` the word after `flag`?
fn follows(args: &[String], flag: &str, value: &str) -> bool {
    args.windows(2)
        .any(|pair| pair[0] == flag && pair[1] == value)
}

/// The prefix has to be the overlay's *destination* and the real one its
/// source, or the session writes straight into what `just wine-prefix`
/// installed — which is the whole thing this exists to prevent.
#[test]
fn overlays_the_real_prefix_onto_the_session_one() {
    let args = args("/home/me/.wine-demarc", "/run/user/1000/x/0", None);
    assert!(follows(&args, "--overlay-src", "/home/me/.wine-demarc"));
    assert!(follows(&args, "--tmp-overlay", "/run/user/1000/x/0"));
    // And never the other way round.
    assert!(!follows(&args, "--tmp-overlay", "/home/me/.wine-demarc"));
}

/// wine names its server socket after the prefix's device and inode, under
/// `/tmp/.wine-<uid>`. A private one there is what makes two sessions two
/// wineservers rather than two names for one.
#[test]
fn gives_the_session_its_own_wineserver_directory() {
    let args = args("/base", "/session", None);
    let socket_dir = format!("/tmp/.wine-{}", uid());
    assert!(follows(&args, "--tmpfs", &socket_dir));
    // 0700, or wineserver refuses it: "must not be accessible by other users".
    let tmpfs = args.iter().position(|a| a == "--tmpfs").expect("a tmpfs");
    assert_eq!(args[tmpfs - 2], "--perms");
    assert_eq!(args[tmpfs - 1], "0700");
}

/// wine's services `setsid` out of any process group they are put in, which is
/// why both backends carry code to hunt them down. A pid namespace is the one
/// thing they cannot leave.
#[test]
fn puts_the_session_in_a_pid_namespace_of_its_own() {
    let args = args("/base", "/session", None);
    assert!(args.iter().any(|a| a == "--unshare-pid"));
    assert!(args.iter().any(|a| a == "--die-with-parent"));
    // A pid namespace showing the host's /proc is worse than none, so the fresh
    // mount has to come after the bind that brought the host's in.
    let bind = args.iter().position(|a| a == "--dev-bind").expect("a bind");
    let proc = args.iter().position(|a| a == "--proc").expect("a /proc");
    assert!(proc > bind);
}

/// A release that ships a `data/` folder or its own `fmod.dll` finds neither
/// from anywhere else.
#[test]
fn starts_the_demo_in_its_own_directory() {
    let with = args("/base", "/session", Some("/demos/thing"));
    assert!(follows(&with, "--chdir", "/demos/thing"));

    let without = args("/base", "/session", None);
    assert!(!without.iter().any(|a| a == "--chdir"));
}

/// `bwrap` chdirs inside the new mount namespace, so a relative directory —
/// which is what `demarc inside/demo.exe` hands us — would fail the whole
/// sandbox and the demo would never start.
#[test]
fn the_working_directory_is_made_absolute() {
    let here = std::env::current_dir().expect("a working directory");
    let args = args("/base", "/session", Some("."));
    assert!(follows(&args, "--chdir", &here.to_string_lossy()));
}

/// The sandbox says which prefix it is itself, so it and whoever spawns it
/// cannot disagree about that.
#[test]
fn names_the_prefix_in_the_environment() {
    let args = args("/base", "/session", None);
    let at = args
        .iter()
        .position(|a| a == "--setenv")
        .expect("a --setenv");
    assert_eq!(args[at + 1], "WINEPREFIX");
    assert_eq!(args[at + 2], "/session");
}

/// Everything before the command, and nothing after it: the demo's own argv is
/// the caller's and is passed through untouched.
#[test]
fn wraps_a_command_without_changing_it() {
    let sandbox = Sandbox {
        prefix: PathBuf::from("/session"),
        argv: args("/base", "/session", None),
    };
    let command = vec!["wine".to_string(), "some demo.exe".to_string()];
    let wrapped = sandbox.wrap(command.clone());

    assert_eq!(wrapped[wrapped.len() - command.len()..], command[..]);
    // The separator is what tells the two halves apart, and there is exactly
    // one of it.
    assert_eq!(wrapped.iter().filter(|a| *a == "--").count(), 1);
    assert_eq!(wrapped[0], BWRAP);
}

/// On unless an entry says otherwise, and off for anything that is not a yes:
/// the point of the key is to be able to turn it off.
#[test]
fn sandboxes_by_default() {
    let asked = |value: &str| {
        wanted(&HashMap::from([(
            META_SANDBOX.to_string(),
            value.to_string(),
        )]))
    };
    assert_eq!(wanted(&HashMap::new()), DEFAULT_SANDBOX);
    assert!(asked("true"));
    assert!(asked("yes"));
    assert!(!asked("false"));
    assert!(!asked("disabled"));
}

/// A prefix that does not exist yet is not one to sandbox: the session would
/// build it inside a tmpfs and throw it away again, paying `wineboot` every
/// time and keeping nothing.
#[test]
fn refuses_a_prefix_that_is_not_there() {
    let missing = std::env::temp_dir().join("demarc-no-such-prefix");
    assert!(prepare(&missing, None).is_err());
}

/// A crash leaves mount points behind. They are empty directories rather than
/// mounts, so this is tidiness — but a live demarc's must survive it.
#[test]
fn sweeps_dead_runs_and_keeps_live_ones() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let mine = dir.path().join(std::process::id().to_string());
    // pid 0 is never a process, so it stands in for a run that is over.
    let dead = dir.path().join("0");
    let other = dir.path().join("not-a-pid");
    for path in [&mine, &dead, &other] {
        std::fs::create_dir(path).expect("a directory");
    }

    sweep(dir.path());

    assert!(
        mine.is_dir(),
        "a live demarc's sandboxes are not ours to take"
    );
    assert!(!dead.exists(), "a dead run's mount points are swept");
    assert!(other.is_dir(), "only pid-named directories are touched");
}
