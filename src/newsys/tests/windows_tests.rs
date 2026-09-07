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

/// A demo that ships its own d3dx9 ships it because wine's builtin one is not
/// the build it was linked against — see `NATIVE_DLLS`. Nobody writes that
/// down in an entry, so it is read off the release itself.
#[test]
fn takes_dll_overrides_from_the_dlls_a_release_ships() {
    let dir = tempfile::tempdir().unwrap();
    let sys = WindowsSystem {};

    let release = dir.path().join("stargazer");
    fs::create_dir_all(&release).unwrap();
    windows_exe(&release, "stargazer.exe");
    write_bytes(&release, "d3dx9_43.dll", b"MZ");
    // Upper case on disk, lower case in the variable: wine matches the module
    // name either way, and one spelling keeps the string predictable.
    write_bytes(&release, "D3DX9_37.DLL", b"MZ");
    // Shipped too, and not ours to override: wine's own is the better one.
    write_bytes(&release, "openal32.dll", b"MZ");
    // Not beside the executable, so not something wine is about to load.
    fs::create_dir_all(release.join("data")).unwrap();
    write_bytes(&release.join("data"), "d3dx9_31.dll", b"MZ");

    let mut wf = WorkFile::new(release.clone());
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(
        wf.get_meta_or(META_DLL_OVERRIDES, ""),
        "d3dx9_37,d3dx9_43=n"
    );

    // What the entry says was written by a person who knew what they meant,
    // and it is the whole variable — adding to it is theirs to do.
    let meta = HashMap::from([(META_DLL_OVERRIDES.to_string(), "d3d9=n,b".to_string())]);
    let mut wf = WorkFile::new_with_meta(release, meta);
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(META_DLL_OVERRIDES, ""), "d3d9=n,b");
}

/// A release with nothing worth overriding gets no variable at all, rather than
/// an empty one.
#[test]
fn a_release_with_no_native_dlls_asks_for_no_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let sys = WindowsSystem {};

    let release = dir.path().join("plain");
    fs::create_dir_all(&release).unwrap();
    windows_exe(&release, "plain.exe");
    write_bytes(&release, "fmod.dll", b"MZ");

    let mut wf = WorkFile::new(release);
    assert!(sys.load(&mut wf).unwrap());
    assert!(!wf.has_meta(META_DLL_OVERRIDES));
}

/// Only one `*`, and it stands for anything or nothing.
#[test]
fn globs_dll_names_loosely_enough_to_be_useful() {
    assert!(glob_match("d3dx9_43.dll", "d3dx9*.dll"));
    assert!(glob_match("D3DX9_43.DLL", "d3dx9*.dll"));
    // The `*` may stand for nothing at all.
    assert!(glob_match("d3dx9.dll", "d3dx9*.dll"));
    // ...but the two ends may not overlap to make one.
    assert!(!glob_match("d3dx9.dl", "d3dx9*.dll"));
    assert!(!glob_match("d3dx10_43.dll", "d3dx9*.dll"));
    assert!(!glob_match("xd3dx9_43.dll", "d3dx9*.dll"));
    // No `*` is a plain comparison.
    assert!(glob_match("fmod.dll", "fmod.dll"));
    assert!(!glob_match("fmodex.dll", "fmod.dll"));
}

/// The captured backend runs the same wine, so it needs the same variable —
/// under the name the core knows it by.
#[test]
fn restates_dll_overrides_as_a_core_option() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "thing.exe");
    let file = WorkFile::new_with_meta(
        exe.clone(),
        HashMap::from([(META_DLL_OVERRIDES.to_string(), "d3dx9_37=n".to_string())]),
    );

    let meta = capture_meta(&file);

    assert_eq!(
        meta.get("gamescope_wine_dll_overrides").map(String::as_str),
        Some("d3dx9_37=n")
    );

    // Nothing asked for, nothing sent: an empty WINEDLLOVERRIDES is not the
    // same as no WINEDLLOVERRIDES.
    let file = WorkFile::new(exe);
    assert!(!capture_meta(&file).contains_key("gamescope_wine_dll_overrides"));
}

/// `wine_gl_compat` is demarc's yes/no; what the core exports is the Mesa
/// variable itself. The translation is what lets an `overrides.toml` entry go on
/// saying the readable thing.
#[test]
fn restates_gl_compat_as_a_mesa_override() {
    let dir = tempfile::tempdir().unwrap();
    let exe = windows_exe(dir.path(), "thing.exe");

    let file = WorkFile::new_with_meta(
        exe.clone(),
        HashMap::from([(META_GL_COMPAT.to_string(), "true".to_string())]),
    );
    assert_eq!(
        capture_meta(&file)
            .get("gamescope_mesa_gl_version_override")
            .map(String::as_str),
        Some(GL_COMPAT_OVERRIDE)
    );

    // A no leaves the variable out altogether: unset is what lets the demo's own
    // profile request stand, which is right for everything that does not need
    // this.
    let file = WorkFile::new_with_meta(
        exe.clone(),
        HashMap::from([(META_GL_COMPAT.to_string(), "false".to_string())]),
    );
    assert!(
        !capture_meta(&file).contains_key("gamescope_mesa_gl_version_override"),
        "an explicit no should ask for nothing"
    );

    let file = WorkFile::new(exe);
    assert!(!capture_meta(&file).contains_key("gamescope_mesa_gl_version_override"));
}
