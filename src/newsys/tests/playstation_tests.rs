use super::*;

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join("psx")
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A data disc with nothing on it the PlayStation would boot — what another
/// console's disc looks like from the outside, down to the MODE2/2352
/// wrapper when `raw` is set.
fn other_disc(dir: &Path, name: &str, raw: bool) -> PathBuf {
    let files = [("DATA.BIN".to_string(), vec![0u8; ISO_SECTOR])];
    let mut image = build_iso(&IsoSpec {
        system_id: "OTHER",
        volume_id: "OTHER",
        files: &files,
        ..Default::default()
    });

    if raw {
        // Wrap each sector the way a cue's bin does: sync pattern, address,
        // XA subheader, user data, then room for the error correction.
        let total = image.len() / ISO_SECTOR;
        let mut sectors = vec![0u8; total * 2352];
        for lba in 0..total {
            let at = lba * 2352;
            sectors[at] = 0;
            sectors[at + 1..at + 12].fill(0xff);
            sectors[at + 15] = 2;
            sectors[at + 24..at + 24 + ISO_SECTOR]
                .copy_from_slice(&image[lba * ISO_SECTOR..][..ISO_SECTOR]);
        }
        image = sectors;
    }

    let path = dir.join(name);
    fs::write(&path, image).unwrap();
    path
}

/// The two markers, one each: a raw dump that still carries Sony's licence
/// text, and a data track whose system area was stripped, leaving only the
/// boot file in the root to go on.
#[test]
fn detects_psx_discs() {
    assert!(is_psx_disc(
        &testdata().join("thisispsx/thisispsx_rc1c_iso.bin")
    ));
    assert!(is_psx_disc(&testdata().join("monophobia/mono_t1.bin")));
    assert!(is_psx_cue(&testdata().join("monophobia/mono.cue")));
}

/// A wrapped executable has to come back out as a disc the same detection
/// accepts, or the release would only load the once.
#[test]
fn wrapped_executable_is_a_psx_disc() {
    let iso = create_psx_iso(&testdata().join("paradox/pdx-051.psx"))
        .unwrap()
        .expect("paradox release is a PS-X EXE");
    assert!(is_psx_disc(&iso));
}

/// Everything that isn't one: another console's disc in either wrapper, a
/// cue naming it, an audio-only cue, and a file that is no disc at all.
#[test]
fn rejects_other_discs() {
    let dir = temp_dir("psx_detect_test");
    let iso = other_disc(&dir, "other.iso", false);
    let bin = other_disc(&dir, "other.bin", true);
    // Both are readable data discs — rejected on what's on them, not
    // because the layout sniff gave up on them.
    assert!(DiscImage::open(&iso).is_some());
    assert!(DiscImage::open(&bin).is_some());
    assert!(!is_psx_disc(&iso));
    assert!(!is_psx_disc(&bin));

    let cue = dir.join("other.cue");
    fs::write(&cue, "FILE \"other.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
    assert!(!is_psx_cue(&cue));

    fs::write(dir.join("track.wav"), b"not really a wav").unwrap();
    let audio = dir.join("audio.cue");
    fs::write(&audio, "FILE \"track.wav\" WAVE\n  TRACK 01 AUDIO\n").unwrap();
    assert!(!is_psx_cue(&audio));

    assert!(!is_psx_disc(&testdata().join("paradox/pdx-051.JPG")));
}
