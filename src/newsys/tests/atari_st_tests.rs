use super::*;

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Pointed at a program on its own, only that program goes on the drive —
/// whatever else shares the directory with it is none of our business.
#[test]
fn bare_program_leaves_its_directory_alone() {
    let dir = tempfile::tempdir().unwrap();
    let prg = dir.path().join("DEMO.EXE");
    write(&prg, &GEMDOS_MAGIC);
    write(&dir.path().join("huge.iso"), b"not ours");

    let wf = build_gemdos_drive(&prg, &prg).unwrap();
    let drive = wf.path.parent().unwrap().join("harddrive");

    assert!(drive.join("AUTO").join(AUTO_PROGRAM).exists());
    assert!(!drive.join("huge.iso").exists());
}

/// A directory release brings its files along, and its own `AUTO` folder is
/// moved aside so ours is the one TOS boots.
#[test]
fn directory_release_is_copied_with_its_auto_moved_aside() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("release");
    let prg = base.join("DEMO.EXE");
    write(&prg, &GEMDOS_MAGIC);
    write(&base.join("data").join("music.snd"), b"tune");
    write(&base.join("auto").join("SWAP.PRG"), b"stub");

    let wf = build_gemdos_drive(&prg, &base).unwrap();
    let drive = wf.path.parent().unwrap().join("harddrive");

    assert!(drive.join("data").join("music.snd").exists());
    assert!(drive.join(DISABLED_AUTO).join("SWAP.PRG").exists());
    assert!(drive.join("AUTO").join(AUTO_PROGRAM).exists());
}

fn exe(dir: &Path, name: &str, size: usize) -> PathBuf {
    let path = dir.join(name);
    let mut bytes = GEMDOS_MAGIC.to_vec();
    bytes.resize(size, 0);
    write(&path, &bytes);
    path
}

/// TalkTalk2: the demo and its readme viewer sit side by side, both named
/// like programs, and the demo is the bigger one.
#[test]
fn biggest_program_of_a_level_wins() {
    let dir = tempfile::tempdir().unwrap();
    let readme = exe(dir.path(), "TLK2READ.PRG", 40_000);
    let demo = exe(dir.path(), "TLKTLK2.PRG", 124_000);

    let exes = [readme, demo.clone()];
    assert_eq!(pick_program(&exes, dir.path(), ""), Some(&demo));
}

/// molz: the 651-byte `.tos` loader is what reads the music and hands it to
/// the 652K part, which is a GEMDOS executable too but named like data.
#[test]
fn program_extension_beats_size() {
    let dir = tempfile::tempdir().unwrap();
    let loader = exe(dir.path(), "molz.tos", 651);
    let part = exe(dir.path(), "part1.bin", 652_000);

    let exes = [part, loader.clone()];
    assert_eq!(pick_program(&exes, dir.path(), ""), Some(&loader));
}

/// Between programs alike in name, the one nearer the top of the release.
#[test]
fn shallowest_program_wins() {
    let dir = tempfile::tempdir().unwrap();
    let top = exe(dir.path(), "DEMO.PRG", 1000);
    let buried = exe(&dir.path().join("PARTS"), "PART1.PRG", 500_000);

    let exes = [buried, top.clone()];
    assert_eq!(pick_program(&exes, dir.path(), ""), Some(&top));
}

/// `boot_file` overrides the guess outright, by name or by path, and a
/// GEMDOS-style path in it still matches.
#[test]
fn boot_file_overrides_the_guess() {
    let dir = tempfile::tempdir().unwrap();
    let big = exe(dir.path(), "BIG.PRG", 500_000);
    let wanted = exe(&dir.path().join("DEMO"), "TLKTLK2.PRG", 1000);
    let exes = [big.clone(), wanted.clone()];

    assert_eq!(
        pick_program(&exes, dir.path(), "tlktlk2.prg"),
        Some(&wanted)
    );
    assert_eq!(
        pick_program(&exes, dir.path(), "DEMO/TLKTLK2.PRG"),
        Some(&wanted)
    );
    assert_eq!(
        pick_program(&exes, dir.path(), "DEMO\\TLKTLK2.PRG"),
        Some(&wanted)
    );
    // A name that isn't there falls back to the guess rather than failing.
    assert_eq!(pick_program(&exes, dir.path(), "GONE.PRG"), Some(&big));
}

/// A program inside the release's own `AUTO` folder is already started by
/// it, so the drive is the folder above and nothing is rearranged.
#[test]
fn program_in_auto_starts_itself() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().join("release");
    let prg = base.join("AUTO").join("DEMO.PRG");
    write(&prg, &GEMDOS_MAGIC);

    let wf = build_gemdos_drive(&prg, &base).unwrap();
    let drive = wf.path.parent().unwrap().join("harddrive");

    assert!(drive.join("AUTO").join("DEMO.PRG").exists());
    assert!(!drive.join("AUTO").join(AUTO_PROGRAM).exists());
    assert!(!drive.join(DISABLED_AUTO).exists());
}
