use super::*;

/// A pack-shaped tree under a temp directory, torn down by [`Drop`].
struct Pack(PathBuf);

impl Pack {
    /// `name` only has to be unique per test; the tree is
    /// `<system>/<monitor>/<flavour>/<preset>.slangp` as the real pack is.
    fn new(name: &str, presets: &[&str]) -> Self {
        let root = std::env::temp_dir().join(format!("demarc-shader-dialog-{name}"));
        let _ = std::fs::remove_dir_all(&root);
        for preset in presets {
            let path = root.join(preset);
            std::fs::create_dir_all(path.parent().expect("has a directory")).expect("mkdir");
            std::fs::write(&path, "#reference \"stock.slangp\"\n").expect("preset");
        }
        Self(root)
    }

    fn browser(&self) -> PresetBrowser {
        PresetBrowser::new(self.0.clone()).expect("pack should open")
    }
}

impl Drop for Pack {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A slice of the real pack, small enough to assert on in full.
const SAMPLE: &[&str] = &[
    "Commodore_Amiga500/Bezel_Black/MBZ_SHARP_STD/NEAR_FLAT_DAY.slangp",
    "Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/NEAR_CURVED_DAY.slangp",
    "Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/NEAR_CURVED_NIGHT.slangp",
    "Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/OVERLAY_FLAT_DAY.slangp",
    "Commodore_Amiga500/Commodore_C1084/NMC_SOFT_RGB/NEAR_CURVED_DAY.slangp",
    "Commodore_C64-Breadbin/Commodore_C1084/MBZ_SHARP_STD/NEAR_CURVED_DAY.slangp",
];

fn raws(level: &Level) -> Vec<&str> {
    level.choices.iter().map(|c| c.raw.as_str()).collect()
}

fn labels(level: &Level) -> Vec<&str> {
    level.choices.iter().map(|c| c.label.as_str()).collect()
}

/// A fresh browser reads only the top level's directory and picks the first of
/// everything, all the way down to a preset that exists.
#[test]
fn opens_on_the_first_preset() {
    let pack = Pack::new("first", SAMPLE);
    let browser = pack.browser();

    assert_eq!(
        raws(&browser.levels[0]),
        ["Commodore_Amiga500", "Commodore_C64-Breadbin"]
    );
    assert_eq!(raws(&browser.levels[1]), ["Bezel_Black", "Commodore_C1084"]);
    assert_eq!(raws(&browser.levels[2]), ["MBZ_SHARP_STD"]);
    assert_eq!(raws(&browser.levels[TYPE]), ["NEAR_FLAT"]);
    assert_eq!(raws(&browser.levels[LIGHT]), ["DAY"]);
    assert_eq!(
        browser.path(),
        Some(pack.0.join(SAMPLE[0])),
        "should compose the first preset of the first branch"
    );
}

/// Each level's choices come from the level above: switching monitor re-reads
/// the flavours under *that* monitor, and so on down.
#[test]
fn a_pick_re_reads_the_levels_below_it() {
    let pack = Pack::new("cascade", SAMPLE);
    let mut browser = pack.browser();

    browser.select(1, 1); // Commodore_C1084
    assert_eq!(
        raws(&browser.levels[2]),
        ["MBZ_SHARP_STD", "NMC_SOFT_RGB"],
        "the other monitor ships a second flavour"
    );
    assert_eq!(raws(&browser.levels[TYPE]), ["NEAR_CURVED", "OVERLAY_FLAT"]);
    assert_eq!(raws(&browser.levels[LIGHT]), ["DAY", "NIGHT"]);

    browser.select(LIGHT, 1);
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[2])));
}

/// A level keeps its pick when the new choices still offer it, so walking
/// across machines stays on the preset the user chose rather than resetting to
/// the first one.
#[test]
fn a_still_valid_pick_survives_a_change_above_it() {
    let pack = Pack::new("keep", SAMPLE);
    let mut browser = pack.browser();

    browser.select(1, 1);
    browser.select(LIGHT, 1); // NIGHT
    browser.select(0, 1); // the C64, which ships only the day preset

    assert_eq!(
        raws(&browser.levels[1]),
        ["Commodore_C1084"],
        "the monitor is still there under the new machine, so it is kept"
    );
    assert_eq!(
        raws(&browser.levels[LIGHT]),
        ["DAY"],
        "NIGHT is not, so that level falls back to its first choice"
    );
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[5])));
}

/// The lighting list belongs to the selected type: `OVERLAY_*` ships day only,
/// and picking it must not leave `NIGHT` selectable (and unopenable).
#[test]
fn lighting_follows_the_selected_type() {
    let pack = Pack::new("lighting", SAMPLE);
    let mut browser = pack.browser();

    browser.select(1, 1);
    browser.select(LIGHT, 1);
    browser.select(TYPE, 1); // OVERLAY_FLAT

    assert_eq!(raws(&browser.levels[LIGHT]), ["DAY"]);
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[3])));
}

/// Opening the dialog over a running preset selects every level of it.
#[test]
fn reveal_selects_an_existing_preset() {
    let pack = Pack::new("reveal", SAMPLE);
    let mut browser = pack.browser();

    let wanted = pack.0.join(SAMPLE[4]);
    assert!(browser.reveal(&wanted));
    assert_eq!(browser.path(), Some(wanted));
    assert_eq!(browser.levels[2].raw(), Some("NMC_SOFT_RGB"));

    // A preset from somewhere else is not this pack's business, and leaves the
    // selection alone.
    let elsewhere = PathBuf::from("shaders/slangp/crt/crt-lottes.slangp");
    assert!(!browser.reveal(&elsewhere));
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[4])));

    // Neither is a path of the right shape naming something the pack does not
    // ship -- but the levels that did match are selected.
    let absent = pack
        .0
        .join("Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/GONE_FLAT_DAY.slangp");
    assert!(!browser.reveal(&absent));
    assert_eq!(browser.levels[1].raw(), Some("Commodore_C1084"));
    assert!(browser.path().is_some_and(|p| p.is_file()));
}

/// Every path the dialog can compose is a file that exists: that is the whole
/// contract, since a pick is applied the moment it is made.
#[test]
fn every_selection_names_a_real_preset() {
    let pack = Pack::new("exhaustive", SAMPLE);
    let mut browser = pack.browser();

    for system in 0..browser.levels[0].choices.len() {
        browser.select(0, system);
        for monitor in 0..browser.levels[1].choices.len() {
            browser.select(1, monitor);
            for flavour in 0..browser.levels[2].choices.len() {
                browser.select(2, flavour);
                for ty in 0..browser.levels[TYPE].choices.len() {
                    browser.select(TYPE, ty);
                    for light in 0..browser.levels[LIGHT].choices.len() {
                        browser.select(LIGHT, light);
                        let path = browser.path().expect("a full selection has a path");
                        assert!(path.is_file(), "{path:?} does not exist");
                    }
                }
            }
        }
    }
}

/// A preset with no lighting half is still selectable; the level that has
/// nothing to offer just drops out of the file name.
#[test]
fn a_name_with_no_lighting_half_still_resolves() {
    let pack = Pack::new("nolight", &["Machine/Monitor/Flavour/PLAIN.slangp"]);
    let browser = pack.browser();

    assert_eq!(raws(&browser.levels[TYPE]), ["PLAIN"]);
    assert!(browser.levels[LIGHT].choices.is_empty());
    assert_eq!(
        browser.path(),
        Some(pack.0.join("Machine/Monitor/Flavour/PLAIN.slangp"))
    );
}

/// Nothing to browse is reported rather than opened: a missing pack (the
/// `shaders/` checkout is gitignored, so a fresh clone has none) and one whose
/// directories hold no presets.
#[test]
fn an_unusable_pack_is_an_error() {
    let missing = std::env::temp_dir().join("demarc-shader-dialog-absent");
    let _ = std::fs::remove_dir_all(&missing);
    assert!(PresetBrowser::new(missing).is_err());

    let empty = Pack::new("empty", &["Machine/Monitor/Flavour/notes.txt"]);
    assert!(PresetBrowser::new(empty.0.clone()).is_err());
}

/// Each level is labelled the way that level's names read: machine and monitor
/// names are words, flavour codes are initialisms and stay as they are, and the
/// halves of a preset's shouted file name become words again.
#[test]
fn labels_suit_the_level() {
    let pack = Pack::new("labels", SAMPLE);
    let mut browser = pack.browser();
    browser.select(1, 1);

    assert_eq!(
        labels(&browser.levels[0]),
        ["Commodore Amiga500", "Commodore C64-Breadbin"]
    );
    assert_eq!(
        labels(&browser.levels[2]),
        ["MBZ_SHARP_STD", "NMC_SOFT_RGB"]
    );
    assert_eq!(
        labels(&browser.levels[TYPE]),
        ["Near Curved", "Overlay Flat"]
    );
    assert_eq!(labels(&browser.levels[LIGHT]), ["Day", "Night"]);
}

#[test]
fn a_preset_name_splits_at_its_last_underscore() {
    assert_eq!(split_stem("NEAR_CURVED_NIGHT"), ("NEAR_CURVED", "NIGHT"));
    assert_eq!(split_stem("PLAIN"), ("PLAIN", ""));
    assert_eq!(split_stem("A_B"), ("A", "B"));
}

/// What the dialog prints under the combo boxes: the preset without the pack
/// root, which is the tail of a `--slangp` argument.
#[test]
fn the_shown_path_is_relative_to_the_pack() {
    let pack = Pack::new("relative", SAMPLE);
    let browser = pack.browser();
    assert_eq!(browser.relative_path().as_deref(), Some(SAMPLE[0]));
}

/// A pack directory names its author first; the dialog names the machines.
#[test]
fn a_pack_is_named_after_its_machines() {
    assert_eq!(pack_label("TheNamec-Commodore"), "Commodore");
    assert_eq!(pack_label("Duimon-Sega_Genesis"), "Sega Genesis");
    assert_eq!(pack_label("Commodore"), "Commodore");
}

/// A dialog over one pack, plus the default collection every dialog has.
fn dialog_over(pack: &Pack) -> ShaderDialog {
    ShaderDialog {
        collections: vec![
            Collection {
                label: "Default".to_owned(),
                browser: None,
            },
            Collection {
                label: "Commodore".to_owned(),
                browser: Some(pack.browser()),
            },
        ],
        ..Default::default()
    }
}

/// Opening over a running preset selects the collection it came from, and
/// every level of it; the default collection has no levels to select, which is
/// what greys its rows out.
#[test]
fn reveal_selects_the_collection_the_preset_came_from() {
    let pack = Pack::new("collections", SAMPLE);
    let mut dialog = dialog_over(&pack);
    assert!(
        dialog.browser().is_none(),
        "opens on the default collection"
    );

    dialog.reveal(&pack.0.join(SAMPLE[4]));
    assert_eq!(dialog.selected, 1);
    assert_eq!(
        dialog.browser().and_then(PresetBrowser::path),
        Some(pack.0.join(SAMPLE[4]))
    );

    // A preset of the pack's shape that it no longer ships is still the pack's,
    // so the collection stays selected rather than falling back to the default.
    dialog.reveal(
        &pack
            .0
            .join("Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/GONE_FLAT_DAY.slangp"),
    );
    assert_eq!(dialog.selected, 1);

    // The built-in shader belongs to no pack, and is the default collection.
    dialog.reveal(Path::new("shaders/slangp/crt/crt-lottes.slangp"));
    assert_eq!(dialog.selected, DEFAULT);
    assert!(dialog.browser().is_none());
}

/// With no pack installed there is still a collection to show: the default,
/// which browses nothing.
#[test]
fn the_default_collection_is_always_there() {
    let found = collections();
    assert_eq!(found[DEFAULT].label, "Default");
    assert!(found[DEFAULT].browser.is_none());
}

/// The real pack, if this checkout has one. Ignored for the same reason
/// `post_process_tests::megabezel_pack_presets_resolve` is: it needs
/// `shaders/` laid out as `docs/SHADERS.md` describes.
#[test]
#[ignore]
fn the_installed_pack_browses() {
    let mut found = collections();
    let pack = found.get_mut(1).expect("no pack installed");
    assert_eq!(pack.label, "Commodore");
    let browser = pack.browser.as_mut().expect("a collection with a tree");

    let wanted = Path::new(PACKS_DIR).join(
        "TheNamec-Commodore/presets/Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/NEAR_CURVED_NIGHT.slangp",
    );
    assert!(browser.reveal(&wanted), "{wanted:?} should be in the pack");
    assert_eq!(
        labels(&browser.levels[0])[browser.levels[0].index],
        "Commodore Amiga500"
    );
    assert_eq!(
        labels(&browser.levels[TYPE])[browser.levels[TYPE].index],
        "Near Curved"
    );
    assert_eq!(
        labels(&browser.levels[LIGHT])[browser.levels[LIGHT].index],
        "Night"
    );
    assert_eq!(browser.path(), Some(wanted));
}
