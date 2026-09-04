use super::*;
use std::io::Cursor;

/// Parse a hunk file written as a list of longwords.
fn parses(longs: &[u32]) -> bool {
    let data: Vec<u8> = longs.iter().flat_map(|l| l.to_be_bytes()).collect();
    let len = data.len() as u64;
    parse_exe(&mut HunkReader {
        inner: Cursor::new(data),
        pos: 0,
        len,
    })
    .is_some()
}

/// One code hunk of two longwords, the smallest thing LoadSeg() accepts.
const MINIMAL: [u32; 11] = [
    HUNK_HEADER,
    0,
    1,
    0,
    0,
    2,
    HUNK_CODE,
    2,
    0x4E71,
    0x4E75,
    HUNK_END,
];

#[test]
fn accepts_minimal_exe() {
    assert!(parses(&MINIMAL));
}

#[test]
fn accepts_trailing_data() {
    let mut file = MINIMAL.to_vec();
    file.extend([0xDEAD, 0xBEEF]);
    assert!(parses(&file));
}

/// A hunk may open with blocks that carry no memory, which LoadSeg() skips
/// wherever it meets them — `eph-fels.exe` puts a HUNK_DEBUG in front of its
/// first HUNK_CODE, and rejecting it costs the real demo to a lesser file.
#[test]
fn accepts_debug_block_before_the_hunk_contents() {
    let mut file = MINIMAL[..6].to_vec();
    file.extend([HUNK_DEBUG, 2, 0xF00D, 0xF00D]);
    file.extend(&MINIMAL[6..]);
    assert!(parses(&file));
}

#[test]
fn accepts_hunk_reserving_more_than_it_loads() {
    let mut file = MINIMAL;
    file[5] = 0x1000;
    assert!(parses(&file));
}

#[test]
fn accepts_relocs_and_symbols() {
    assert!(parses(&[
        HUNK_HEADER,
        0,
        2,
        0,
        1,
        2,
        1,
        HUNK_CODE,
        2,
        0x4E71_4E71,
        0x4E75_0000,
        HUNK_RELOC32,
        1,
        1,
        0,
        0,
        HUNK_SYMBOL,
        1,
        0x6D61,
        0,
        0,
        HUNK_END,
        HUNK_BSS,
        1,
        HUNK_END,
    ]));
}

/// HUNK_END is a terminator LoadSeg() waits for rather than one it needs,
/// and linkers do leave it out — `dcs-klone.exe` runs its first hunk
/// straight into the second, and is a whole demo to lose over a marker.
#[test]
fn accepts_hunk_running_into_the_next_without_an_end() {
    assert!(parses(&[
        HUNK_HEADER,
        0,
        2,
        0,
        1,
        2,
        1,
        HUNK_CODE,
        2,
        0x4E71_4E71,
        0x4E75_0000,
        HUNK_BSS,
        1,
        HUNK_END,
    ]));
}

#[test]
fn rejects_wrong_magic() {
    let mut file = MINIMAL;
    file[0] = HUNK_CODE;
    assert!(!parses(&file));
}

#[test]
fn rejects_truncated_hunk() {
    assert!(!parses(&MINIMAL[..MINIMAL.len() - 2]));
}

#[test]
fn rejects_missing_hunk_end() {
    assert!(!parses(&MINIMAL[..MINIMAL.len() - 1]));
}

#[test]
fn rejects_block_bigger_than_reserved() {
    let mut file = MINIMAL;
    file[5] = 1;
    assert!(!parses(&file));
}

#[test]
fn rejects_unknown_block() {
    let mut file = MINIMAL;
    file[6] = 0x1234;
    assert!(!parses(&file));
}

#[test]
fn rejects_reloc_to_missing_hunk() {
    assert!(!parses(&[
        HUNK_HEADER,
        0,
        1,
        0,
        0,
        2,
        HUNK_CODE,
        2,
        0x4E71_4E71,
        0x4E75_0000,
        HUNK_RELOC32,
        1,
        1,
        0,
        0,
        HUNK_END,
    ]));
}

/// `--boot-file`, and an override's `boot`, leave the work file pointing at
/// one program inside a release that is already unpacked. The rest of the
/// release still has to go into the drive: the program loads its parts and
/// its music off it.
#[test]
fn a_named_boot_file_still_brings_the_release_along() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let exe: Vec<u8> = MINIMAL.iter().flat_map(|l| l.to_be_bytes()).collect();
    fs::write(dir.path().join("demo"), &exe).unwrap();
    fs::write(dir.path().join("music.mod"), b"data").unwrap();

    let mut file = WorkFile::new(dir.path().join("demo"));
    file.set_meta(RELEASE_DIR, dir.path().to_string_lossy());
    assert!(AmigaSystem::default().load(&mut file).unwrap());

    assert!(file.path.join("music.mod").is_file(), "data files come too");
    assert_eq!(
        fs::read_to_string(file.path.join("s/startup-sequence")).unwrap(),
        "echo \"Loading...\"\ndemo\n"
    );
}

/// Without a release around it — a loose executable the user pointed at —
/// only the program itself goes in, under the name the generated
/// startup-sequence calls. Copying its directory could be anything at all.
#[test]
fn a_loose_executable_goes_in_on_its_own() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let exe: Vec<u8> = MINIMAL.iter().flat_map(|l| l.to_be_bytes()).collect();
    fs::write(dir.path().join("demo"), &exe).unwrap();
    fs::write(dir.path().join("unrelated"), b"data").unwrap();

    let mut file = WorkFile::new(dir.path().join("demo"));
    assert!(AmigaSystem::default().load(&mut file).unwrap());

    assert!(file.path.join("amiga_file").is_file());
    assert!(!file.path.join("unrelated").exists());
}

/// A release that boots itself keeps its own startup-sequence, so the
/// assigns have to be inserted in front of it rather than replace it.
#[test]
fn inserts_assigns_before_an_existing_startup_sequence() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    fs::create_dir_all(dir.path().join("s")).unwrap();
    let startup = dir.path().join("s/startup-sequence");
    fs::write(&startup, "demo\n").unwrap();

    let mut file = WorkFile::new(dir.path());
    file.set_meta("assign", "data=dh0:stuff;musik=dh0:mod");
    patch_startup_sequence(&mut file, &startup).unwrap();

    // Patched in a temp copy, the release itself untouched.
    assert!(file.is_temporary());
    assert_eq!(fs::read_to_string(&startup).unwrap(), "demo\n");
    assert_eq!(
        fs::read_to_string(file.path.join("s/startup-sequence")).unwrap(),
        "C:Assign data: dh0:stuff\nC:Assign musik: dh0:mod\ndemo\n"
    );
}

/// A disk whose file names puae's file system refuses has to be handed to
/// amiberry, which reads them — the check is what keeps `3d-demo.adf` from
/// booting into nothing.
#[test]
fn a_name_puae_refuses_picks_amiberry() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    fs::write(dir.path().join("Har vi røget hash?"), b"data").unwrap();
    assert!(has_uae_illegal_name(dir.path()));

    let plain = tempfile::Builder::new().tempdir().unwrap();
    fs::create_dir(plain.path().join("s")).unwrap();
    fs::write(plain.path().join("s/startup-sequence"), b"demo\n").unwrap();
    // Non-ASCII on its own is fine: puae only refuses its `evilchars`.
    fs::write(plain.path().join("Bjørn"), b"data").unwrap();
    assert!(!has_uae_illegal_name(plain.path()));
}

#[test]
fn rejects_missing_hunk_block() {
    let mut file = MINIMAL.to_vec();
    // Two hunks announced, only one present.
    file[2] = 2;
    file[4] = 1;
    file.insert(6, 1);
    assert!(!parses(&file));
}
