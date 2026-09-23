use super::*;

#[test]
fn claims_protracker_modules_either_way_round() {
    // The two naming conventions a module arrives under.
    assert!(is_protracker_module(Path::new("enigma.mod")));
    assert!(is_protracker_module(Path::new("mod.enigma")));
    assert!(is_protracker_module(Path::new("/a/dir/MOD.Enigma")));
    // 15-sample modules and the other names they go by.
    assert!(is_protracker_module(Path::new("tune.stk")));
    assert!(is_protracker_module(Path::new("nst.tune")));

    // Other trackers, and other music [`MusicSystem`] claims, stay with MusicEmu.
    assert!(!is_protracker_module(Path::new("tune.xm")));
    assert!(!is_protracker_module(Path::new("tune.sid")));
    assert!(!is_protracker_module(Path::new("mdat.tune")));
    // A module directory, not a module.
    assert!(!is_protracker_module(Path::new("mods")));
}
