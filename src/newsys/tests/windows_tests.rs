use super::*;
#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
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
    // Only where there is a wine to run it.
    assert_eq!(sys.can_load(&win), CAN_RUN_WINDOWS);

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

/// A release directory holding a Windows program is the release, and on
/// Linux it is ours to start.
#[test]
#[cfg(target_os = "linux")]
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
    assert_eq!(
        sys.default_meta().get(crate::wine_emu::META_RES),
        Some(&"800x600")
    );
}

/// A Windows release often names the size it was built for, and that name
/// is the only place the size is written down.
#[test]
#[cfg(target_os = "linux")]
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
    assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "1920x1080");

    // What the entry says was decided by a person, and beats a file name.
    let meta = HashMap::from([(crate::wine_emu::META_RES.to_string(), "800x600".to_string())]);
    let mut wf = WorkFile::new_with_meta(release, meta);
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "800x600");

    // The same release, spelled the other way.
    let elevated = dir.path().join("elevated");
    fs::create_dir_all(&elevated).unwrap();
    write_bytes(&elevated, "elevated_1440_900.exe", &pe);
    let mut wf = WorkFile::new(elevated);
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "1440x900");

    // A DOS program is not this system's, so nothing here fills anything
    // in for it - it runs under DOSBox, which has no such setting.
    let dos = dir.path().join("dos");
    fs::create_dir_all(&dos).unwrap();
    let mut mz = vec![0u8; 0x80];
    mz[..2].copy_from_slice(b"MZ");
    write_bytes(&dos, "demo_640x480.exe", &mz);
    let mut wf = WorkFile::new(dos);
    assert!(!sys.load(&mut wf).unwrap());
    assert!(!wf.has_meta(crate::wine_emu::META_RES));
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
