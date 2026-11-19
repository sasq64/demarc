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

#[test]
fn characters_map_to_the_c64_key_that_types_them() {
    use retro_core::libretro::*;
    assert_eq!(char_key('a'), Some((RETROK_a, false)));
    assert_eq!(char_key('A'), Some((RETROK_a, true)));
    assert_eq!(char_key('"'), Some((RETROK_2, true)));
    assert_eq!(char_key('@'), Some((RETROK_LEFTBRACKET, false)));
    assert_eq!(char_key('['), Some((RETROK_SEMICOLON, true)));
    assert_eq!(char_key('_'), Some((RETROK_BACKQUOTE, false)));
    assert_eq!(char_key('~'), Some((RETROK_DELETE, false)));
    assert_eq!(char_key('{'), None);
}

#[test]
fn boot_events_follow_the_cartridge_and_boot_name() {
    let (_dir, mut file) = prg_release("");
    assert!(boot_events(&file).is_empty());
    file.set_meta("vice_cartridge", "rr38ppal-auto.crt");
    assert_eq!(boot_events(&file), FAST_LOAD_EVENTS.to_vec());
    file.set_meta(BOOT_NAME, "AB*");
    use retro_core::libretro::*;
    assert_eq!(
        boot_events(&file),
        vec![
            (50, InputEvent::KeyState(RETROK_LSHIFT, true)),
            (51, InputEvent::Key(RETROK_5)),
            (54, InputEvent::KeyState(RETROK_LSHIFT, false)),
            (56, InputEvent::Key(RETROK_0)),
            (60, InputEvent::Key(RETROK_SEMICOLON)),
            (64, InputEvent::Key(RETROK_a)),
            (68, InputEvent::Key(RETROK_b)),
            (72, InputEvent::Key(RETROK_RIGHTBRACKET)),
            (76, InputEvent::Key(RETROK_RETURN)),
        ]
    );
}
