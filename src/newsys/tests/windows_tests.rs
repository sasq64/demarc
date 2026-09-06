use super::*;
use std::fs;

fn write_bytes(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

/// A Windows program is an MZ like any other; what makes it one is the PE
/// image the stub points at.
#[test]
fn tells_a_windows_program_from_a_dos_one() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, body: &[u8]| {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    };
    let sys = WindowsSystem {};

    let mut win = vec![0u8; 0x100];
    win[..2].copy_from_slice(b"MZ");
    win[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    win[0x80..0x84].copy_from_slice(b"PE\0\0");
    let win = write("setup32.exe", &win);
    assert!(is_windows_program(&win));
    assert!(sys.can_load(&win));

    // A 64K intro packs the two headers into one: `e_lfanew` points at
    // 0x0c, so the PE header's own fields make up the rest of the DOS
    // header. Well inside it, and still a Windows program.
    let mut tiny = vec![0u8; 0x1000];
    tiny[..2].copy_from_slice(b"MZ");
    tiny[0x0c..0x10].copy_from_slice(b"PE\0\0");
    tiny[0x3c..0x40].copy_from_slice(&0x0cu32.to_le_bytes());
    let tiny = write("intro64k.exe", &tiny);
    assert!(is_windows_program(&tiny));

    // A plain DOS executable, and a DOS extender (LE/LX behind the stub):
    // neither is ours.
    let mut dos = vec![0u8; 0x80];
    dos[..2].copy_from_slice(b"MZ");
    let dos = write("demo.exe", &dos);
    assert!(!is_windows_program(&dos));
    assert!(!sys.can_load(&dos));

    let mut dos4gw = vec![0u8; 0x100];
    dos4gw[..2].copy_from_slice(b"MZ");
    dos4gw[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    dos4gw[0x80..0x82].copy_from_slice(b"LE");
    let dos4gw = write("dos4gw.exe", &dos4gw);
    assert!(!is_windows_program(&dos4gw));

    // An offset pointing past the end of the file is a DOS program with a
    // field it never set, not a Windows one whose image we failed to find.
    let mut stub = vec![0u8; 0x80];
    stub[..2].copy_from_slice(b"MZ");
    stub[0x3c..0x40].copy_from_slice(&0x1000u32.to_le_bytes());
    let stub = write("stub.exe", &stub);
    assert!(!is_windows_program(&stub));

    // Not an executable at all.
    let text = write("notes.exe", b"just a text file\n");
    assert!(!is_windows_program(&text));
}

/// A release directory holding a Windows program is the release, and it is
/// ours to start.
#[test]
fn claims_a_windows_release_for_wine() {
    let dir = tempfile::tempdir().unwrap();
    let sys = WindowsSystem {};

    let release = dir.path().join("kotpg");
    fs::create_dir_all(&release).unwrap();
    let mut pe = vec![0u8; 0x100];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    // The one thing in the release nobody wants to start, reached first.
    write_bytes(&release, "install.exe", &pe);
    write_bytes(&release, "kotpg.exe", &pe);

    let mut wf = WorkFile::new(release.clone());
    assert!(sys.load(&mut wf).unwrap());
    assert!(wf.path.ends_with("kotpg.exe"), "picked {:?}", wf.path);

    // The size the dialog driver is told to pick, unless an entry says
    // otherwise - see `crate::wine_emu`.
    assert_eq!(sys.default_meta().get(META_RES), Some(&"800x600"));
}

/// A Windows release often names the size it was built for, and that name
/// is the only place the size is written down.
#[test]
fn takes_the_resolution_out_of_a_windows_program_name() {
    let dir = tempfile::tempdir().unwrap();
    let sys = WindowsSystem {};
    let mut pe = vec![0u8; 0x100];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");

    let release = dir.path().join("fr08");
    fs::create_dir_all(&release).unwrap();
    write_bytes(&release, "fr08_1920x1080.exe", &pe);

    let mut wf = WorkFile::new(release.clone());
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(META_RES, ""), "1920x1080");

    // What the entry says was decided by a person, and beats a file name.
    let meta = HashMap::from([(META_RES.to_string(), "800x600".to_string())]);
    let mut wf = WorkFile::new_with_meta(release, meta);
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(META_RES, ""), "800x600");

    // The same release, spelled the other way.
    let elevated = dir.path().join("elevated");
    fs::create_dir_all(&elevated).unwrap();
    write_bytes(&elevated, "elevated_1440_900.exe", &pe);
    let mut wf = WorkFile::new(elevated);
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(META_RES, ""), "1440x900");

    // A DOS program is not this system's, so nothing here fills anything
    // in for it - it runs under DOSBox, which has no such setting.
    let dos = dir.path().join("dos");
    fs::create_dir_all(&dos).unwrap();
    let mut mz = vec![0u8; 0x80];
    mz[..2].copy_from_slice(b"MZ");
    write_bytes(&dos, "demo_640x480.exe", &mz);
    let mut wf = WorkFile::new(dos);
    assert!(!sys.load(&mut wf).unwrap());
    assert!(!wf.has_meta(META_RES));
}

/// The scan has to tell a screen mode from every other reason two numbers
/// end up next to each other in a name.
#[test]
fn reads_a_resolution_only_where_a_name_holds_one() {
    let res = |name: &str| res_from_name(Path::new(name));

    assert_eq!(res("bla_1920x1080.exe").as_deref(), Some("1920x1080"));
    assert_eq!(res("demo-640X480.exe").as_deref(), Some("640x480"));
    // Digits running straight into the rest of the name are still digits.
    assert_eq!(res("vga320x200.exe").as_deref(), Some("320x200"));
    assert_eq!(res("intro_512x384_final.exe").as_deref(), Some("512x384"));

    // The same sizes spelled with an underscore between them.
    assert_eq!(res("elevated_1920_1080.exe").as_deref(), Some("1920x1080"));
    assert_eq!(res("elevated_1280_720.exe").as_deref(), Some("1280x720"));
    assert_eq!(res("demo_800_600_final.exe").as_deref(), Some("800x600"));
    // With both to go on, the `x` is the one that means a size.
    assert_eq!(res("party_2009_640x480.exe").as_deref(), Some("640x480"));

    // Not sizes: a pack count, a version, a hex address, a texture.
    assert_eq!(res("pack2x2.exe"), None);
    assert_eq!(res("demo_2_1.exe"), None);
    assert_eq!(res("loader_0x1000.exe"), None);
    assert_eq!(res("atlas_16384x16384.exe"), None);
    // Digits on one side of the separator only.
    assert_eq!(res("directx9.exe"), None);
    assert_eq!(res("64x.exe"), None);
    assert_eq!(res("demo_1024_final.exe"), None);
    assert_eq!(res("demo.exe"), None);
}

/// A Windows program, on disk, so the command built for it can be checked
/// against a path that really exists.
fn windows_exe(dir: &Path, name: &str) -> PathBuf {
    let mut pe = vec![0u8; 0x100];
    pe[..2].copy_from_slice(b"MZ");
    pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    write_bytes(dir, name, &pe)
}

/// The words of a `gamescope_command`, as the core will split them again.
fn argv(meta: &HashMap<String, String>) -> Vec<String> {
    meta.get("gamescope_command")
        .expect("a command")
        .split(ARG_SEPARATOR)
        .map(str::to_string)
        .collect()
}

/// An entry says `wine_res`; the core says `gamescope_resolution`. The
/// translation happens in one place so an `overrides.toml` written for the
/// on-top backend still means the same thing to the captured one.
#[test]
fn restates_wine_settings_as_core_options() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "thing.exe");
    let file = WorkFile::new_with_meta(
        exe.clone(),
        HashMap::from([(META_RES.to_string(), "640x480".to_string())]),
    );

    let meta = capture_meta(&file);

    assert_eq!(
        meta.get("gamescope_resolution").map(String::as_str),
        Some("640x480")
    );
    // Both backends share a prefix, so a release prepared under one is prepared
    // under the other.
    assert!(meta.contains_key("gamescope_wineprefix"));
    // The original key survives: it is still what the entry said.
    assert_eq!(meta.get(META_RES).map(String::as_str), Some("640x480"));

    // Not `wine <exe>`, which is a demo sitting on its setup dialog: the whole
    // command the on-top backend would have run.
    let args = argv(&meta);
    assert_eq!(args[0], "wine");
    let exe = exe.canonicalize().unwrap().to_string_lossy().into_owned();
    match args.iter().position(|a| a == "--launch") {
        // With the driver in the command, the demo is what the driver launches,
        // and the size demarc asked for is the size it presses for.
        Some(launch) => {
            assert_eq!(args[launch + 1], exe);
            let prefer = args.iter().position(|a| a == "--prefer").expect("--prefer");
            assert_eq!(args[prefer + 1], "640x480");
        }
        // No driver built into this checkout: the demo is the command, and the
        // dialog is somebody else's problem. See `wine_emu::autodlg`.
        None => assert_eq!(args, vec!["wine".to_string(), exe]),
    }
}

/// `wine_res=pick` is not a size — it means "let whoever is watching answer the
/// dialog" — so the size the session gets is the backend's own, big enough to
/// hold whatever they pick, and the driver is told to press nothing.
#[test]
fn a_picked_dialog_gets_a_session_big_enough_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "thing.exe");
    let file = WorkFile::new_with_meta(
        exe,
        HashMap::from([(META_RES.to_string(), crate::wine_emu::PICK.to_string())]),
    );

    let meta = capture_meta(&file);

    assert_eq!(
        meta.get("gamescope_resolution").map(String::as_str),
        Some("1920x1200")
    );
    let args = argv(&meta);
    assert!(!args.contains(&"--prefer".to_string()));
    if args.iter().any(|a| a == "--launch") {
        assert!(args.contains(&"--no-go".to_string()));
    }
}

/// A release's path is the one thing in the command that demarc did not choose,
/// and demo filenames are full of spaces. Splitting the command back up on them
/// would tear such a path in half, so the words are held apart by something a
/// path cannot contain.
#[test]
fn a_path_with_spaces_in_it_stays_one_argument() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "second reality (final).exe");
    let file = WorkFile::new(exe.clone());

    let args = argv(&capture_meta(&file));

    let exe = exe.canonicalize().unwrap().to_string_lossy().into_owned();
    assert!(args.contains(&exe), "{args:?}");
}

/// `wine_desktop` is an entry's word for `explorer /desktop=`, and it has to
/// reach the captured session as such: the core's own option for it does
/// nothing, and the command is where the desktop actually lives.
#[test]
fn a_virtual_desktop_reaches_the_captured_session() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "kotpg.exe");
    let file = WorkFile::new_with_meta(
        exe,
        HashMap::from([
            (META_DESKTOP.to_string(), "true".to_string()),
            (META_RES.to_string(), "800x600".to_string()),
        ]),
    );

    let args = argv(&capture_meta(&file));

    assert_eq!(args[0], "wine");
    assert_eq!(args[1], "explorer");
    assert_eq!(args[2], "/desktop=demarc,800x600");
}

/// A value set by hand — `-x gamescope_command=...`, which is how the core gets
/// pointed at a client that is not wine — must survive the translation.
#[test]
fn an_explicit_option_beats_the_translation() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "thing.exe");
    let file = WorkFile::new_with_meta(
        exe,
        HashMap::from([
            (META_RES.to_string(), "800x600".to_string()),
            ("gamescope_resolution".to_string(), "1280x720".to_string()),
            ("gamescope_command".to_string(), "glxgears".to_string()),
        ]),
    );

    let meta = capture_meta(&file);

    assert_eq!(
        meta.get("gamescope_resolution").map(String::as_str),
        Some("1280x720")
    );
    assert_eq!(
        meta.get("gamescope_command").map(String::as_str),
        Some("glxgears")
    );
}

/// A release that has gone missing has no command to build, and must not take
/// the session down with it: the core is left to make what it can of the path.
#[test]
fn a_missing_release_still_gets_a_command() {
    let file = WorkFile::new(PathBuf::from("/no/such/demo.exe"));

    let meta = capture_meta(&file);

    assert_eq!(
        meta.get("gamescope_command").map(String::as_str),
        Some("wine")
    );
    assert!(!meta.contains_key("gamescope_resolution"));
}
