use super::*;

#[test]
fn picks_the_tracker_for_a_module_either_way_round() {
    // The two naming conventions a module arrives under.
    assert_eq!(tracker_core(Path::new("enigma.mod")), Some(PROTRACKER_CORE));
    assert_eq!(tracker_core(Path::new("mod.enigma")), Some(PROTRACKER_CORE));
    assert_eq!(
        tracker_core(Path::new("/a/dir/MOD.Enigma")),
        Some(PROTRACKER_CORE)
    );
    // 15-sample modules and the other names they go by.
    assert_eq!(tracker_core(Path::new("tune.stk")), Some(PROTRACKER_CORE));
    assert_eq!(tracker_core(Path::new("nst.tune")), Some(PROTRACKER_CORE));

    // What the Fasttracker II clone loads.
    assert_eq!(tracker_core(Path::new("tune.xm")), Some(FASTTRACKER_CORE));
    assert_eq!(tracker_core(Path::new("XM.tune")), Some(FASTTRACKER_CORE));
    assert_eq!(tracker_core(Path::new("tune.s3m")), Some(FASTTRACKER_CORE));

    // Other music [`MusicSystem`] claims stays with MusicEmu.
    assert_eq!(tracker_core(Path::new("tune.sid")), None);
    assert_eq!(tracker_core(Path::new("mdat.tune")), None);
    // A module directory, not a module.
    assert_eq!(tracker_core(Path::new("mods")), None);
}

#[test]
fn a_song_name_has_its_type_on_either_end() {
    let name = SongName::parse("MOD.Enigma").unwrap();
    assert_eq!(
        (name.kind, name.title, name.prefixed),
        ("MOD", "Enigma", true)
    );
    let name = SongName::parse("enigma.mod").unwrap();
    assert_eq!(
        (name.kind, name.title, name.prefixed),
        ("mod", "enigma", false)
    );
    // Only a known type counts as a prefix, and a known extension beats it.
    assert!(!SongName::parse("the.song.mod").unwrap().prefixed);
    assert!(!SongName::parse("mod.acker.nsf").unwrap().prefixed);
    assert!(SongName::parse("nodot").is_none());
}

#[test]
fn a_secondary_file_is_named_like_the_song() {
    let secondary = |name| SongName::parse(name).unwrap().secondary();
    assert_eq!(secondary("mdat.a.b c").as_deref(), Some("smpl.a.b c"));
    assert_eq!(secondary("a.b c.mdat").as_deref(), Some("a.b c.smpl"));
    assert_eq!(secondary("tune.mod"), None);
}
