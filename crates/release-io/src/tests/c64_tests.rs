use super::*;
use std::path::PathBuf;

fn prg_release(tags: &str) -> (tempfile::TempDir, WorkFile) {
    // Not the tempdir itself: its ".tmp" name hides it from `walk_dir`.
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("release");
    fs::create_dir(&release).unwrap();
    fs::write(
        release.join("demo.prg"),
        [1, 8, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    )
    .unwrap();
    let meta = [("tags".to_string(), tags.to_string())].into();
    let file = WorkFile::new_with_meta(release, meta);
    (dir, file)
}

#[test]
fn two_sids_set_the_extra_sid_option() {
    let (_dir, mut file) = prg_release("2sid");
    assert!(prepare(&mut file, false).unwrap());
    assert_eq!(file.get_meta_or("vice_sid_extra", ""), "0xd420");
    assert!(!file.has_meta("save_dir"));
    assert_eq!(file.get_meta_or("vice_reset", ""), "hard");
}

#[test]
fn three_sids_go_in_a_vicerc_in_the_save_dir() {
    let (_dir, mut file) = prg_release("3sid");
    assert!(prepare(&mut file, false).unwrap());
    assert!(!file.has_meta("vice_sid_extra"));
    let saves = PathBuf::from(file.get_meta_or("save_dir", ""));
    let rc = fs::read_to_string(saves.join("vicerc")).unwrap();
    assert!(rc.starts_with("[C64SC]\n"));
    assert!(rc.contains("SidStereo=2\n"));
    assert_eq!(file.path.extension().unwrap(), "prg");
}

#[test]
fn bootable_files_skip_what_a_c64_cannot_start() {
    let (_dir, file) = prg_release("");
    fs::write(file.path.join("disk.d64"), [0; 16]).unwrap();
    fs::write(file.path.join("readme.txt"), b"hello").unwrap();
    let names: Vec<_> = bootable_files(&file.path)
        .unwrap()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["demo.prg", "disk.d64"]);
}
