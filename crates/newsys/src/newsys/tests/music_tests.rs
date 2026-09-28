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
