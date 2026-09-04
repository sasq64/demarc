use super::*;

/// A cue naming an MP3 track has to come back naming a WAV one, since no
/// core here decodes MP3 — and the same rewrite has to fix the data track's
/// spelling, which this sheet gives in upper case while the file on disk is
/// lower.
///
/// Runs against the real cache, like [`super::super::playstation`]'s disc
/// test, so it costs a ~15s transcode the first time and nothing after.
#[test]
fn rewrites_an_mp3_cue_to_wav() {
    let cue = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/psx/monophobia/mono.cue");
    let out = prepare_disc(&cue)
        .unwrap()
        .expect("a sheet naming an MP3 needs preparing");

    let text = fs::read_to_string(&out).unwrap();
    assert!(
        text.contains("FILE \"mono_t2.wav\" WAVE"),
        "the MP3 track should be transcoded and renamed: {text}"
    );
    assert!(
        !text.to_uppercase().contains(".MP3"),
        "no MP3 should be left"
    );
    // The data track keeps its kind but picks up the on-disk spelling.
    assert!(
        text.contains("FILE \"mono_t1.bin\" BINARY"),
        "the data track should be recased: {text}"
    );

    let dir = out.parent().unwrap();
    assert!(dir.join("mono_t1.bin").is_file());
    // Decoded CD audio, so it must be a RIFF/WAVE file and much larger than
    // the MP3 it came from.
    let wav = dir.join("mono_t2.wav");
    let head = fs::read(&wav).unwrap();
    assert_eq!(&head[..4], b"RIFF");
    assert_eq!(&head[8..12], b"WAVE");
    assert!(head.len() > 5_873_022);

    // Second call is a cache hit on the same entry, with no re-transcode.
    assert_eq!(prepare_disc(&cue).unwrap().unwrap(), out);
}

fn spec_files(files: &[(&str, usize)]) -> Vec<(String, Vec<u8>)> {
    files
        .iter()
        .map(|(name, len)| (iso_name(name).unwrap(), vec![b'x'; *len]))
        .collect()
}

/// What the writer puts down, the reader has to find again — including the
/// case where the root directory outgrows a single sector, which is the one
/// place the record packing can go wrong.
#[test]
fn round_trips_a_root_directory() {
    let names: Vec<String> = (0..80).map(|i| format!("FILE{i:04}.BIN")).collect();
    let files = spec_files(&names.iter().map(|n| (n.as_str(), 100)).collect::<Vec<_>>());
    let image = build_iso(&IsoSpec {
        system_id: "TEST",
        volume_id: "TEST",
        files: &files,
        ..Default::default()
    });
    // 80 records of 46 bytes plus the two 34-byte ones is past 2048.
    assert!(image.len() / ISO_SECTOR > 20 + 1 + 80);

    let dir = std::env::temp_dir().join("demarc_iso_roundtrip");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.iso");
    fs::write(&path, &image).unwrap();

    let mut disc = DiscImage::open(&path).expect("written image is a data disc");
    assert_eq!(disc.root_names(), names);
}

/// A sheet is only worth loading if everything it names is there, and the
/// track it names is worth loading through it — including when the sheet's
/// spelling of the name isn't the one on disk, which is the usual state of
/// a disc unpacked onto a case-sensitive filesystem.
#[test]
fn finds_the_cue_for_a_track() {
    let dir = std::env::temp_dir().join("demarc_cue_track_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("game.bin"), b"data track").unwrap();
    fs::write(dir.join("game.wav"), b"audio track").unwrap();

    let full = dir.join("game.cue");
    fs::write(
        &full,
        "FILE \"GAME.BIN\" BINARY\n  TRACK 01 MODE2/2352\n\
         FILE \"GAME.WAV\" WAVE\n  TRACK 02 AUDIO\n",
    )
    .unwrap();
    assert!(cue_is_complete(&full));
    assert_eq!(cue_for_track(&dir.join("game.bin")).as_ref(), Some(&full));

    // The audio track was never kept, so nothing can open this sheet — and
    // the bare track beside it must not be loaded through it.
    let broken = dir.join("aa-broken.cue");
    fs::write(
        &broken,
        "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n\
         FILE \"gone.wav\" WAVE\n  TRACK 02 AUDIO\n",
    )
    .unwrap();
    assert!(!cue_is_complete(&broken));
    // Sorts ahead of `game.cue`, so a sheet picked without the check would
    // be this one.
    assert_eq!(cue_for_track(&dir.join("game.bin")).as_ref(), Some(&full));

    // A sheet naming no files at all describes no disc.
    let empty = dir.join("empty.cue");
    fs::write(&empty, "REM nothing here\n").unwrap();
    assert!(!cue_is_complete(&empty));

    // A track no sheet mentions is loaded as itself.
    fs::write(dir.join("other.iso"), b"unrelated").unwrap();
    assert_eq!(cue_for_track(&dir.join("other.iso")), None);
}

/// Level 1 is 8.3 and upper case, and a name that misses is one the boot
/// ROM would never see — so it has to come back as `None`, not mangled.
#[test]
fn checks_iso_names() {
    assert_eq!(iso_name("Test.prg").as_deref(), Some("TEST.PRG"));
    assert_eq!(iso_name("SOUND9V3.z80").as_deref(), Some("SOUND9V3.Z80"));
    assert_eq!(iso_name("IPL").as_deref(), Some("IPL"));
    assert_eq!(iso_name("toolonganame.bin"), None);
    assert_eq!(iso_name("name.toolong"), None);
    assert_eq!(iso_name("has space.bin"), None);
    assert_eq!(iso_name(".prg"), None);
}
