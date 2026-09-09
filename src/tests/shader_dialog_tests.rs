use super::*;

/// The Mega Bezel pack's pattern, which is the deepest shape the dialog has to
/// browse: three directory levels and two wildcards in the file name.
const PACK: &str = "<System>/<Monitor>/<Shader>/<Type>_<Time>.slangp";

/// A tree under a temp directory, torn down by [`Drop`].
struct Pack(PathBuf);

impl Pack {
    /// `name` only has to be unique per test.
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
        self.browse(PACK)
    }

    fn browse(&self, pattern: &str) -> PresetBrowser {
        PresetBrowser::new(self.0.clone(), pattern).expect("pattern should match something")
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

/// The levels of `PACK`: three directories, then the two halves of the file
/// name.
const TYPE: usize = 3;
const TIME: usize = 4;

fn raws(level: &Level) -> Vec<&str> {
    level.choices.iter().map(|c| c.raw.as_str()).collect()
}

fn labels(level: &Level) -> Vec<&str> {
    level.choices.iter().map(|c| c.label.as_str()).collect()
}

/// A pattern's wildcards are its combo boxes, named and ordered as it writes
/// them.
#[test]
fn the_tags_name_the_levels() {
    let pack = Pack::new("tags", SAMPLE);
    let browser = pack.browser();
    assert_eq!(browser.names, ["System", "Monitor", "Shader", "Type", "Time"]);
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
    assert_eq!(raws(&browser.levels[TYPE]), ["NEAR"]);
    assert_eq!(raws(&browser.levels[TIME]), ["FLAT_DAY"]);
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
    assert_eq!(raws(&browser.levels[TYPE]), ["NEAR", "OVERLAY"]);
    assert_eq!(raws(&browser.levels[TIME]), ["CURVED_DAY", "CURVED_NIGHT"]);

    browser.select(TIME, 1);
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
    browser.select(TIME, 1); // CURVED_NIGHT
    browser.select(0, 1); // the C64, which ships only the day preset

    assert_eq!(
        raws(&browser.levels[1]),
        ["Commodore_C1084"],
        "the monitor is still there under the new machine, so it is kept"
    );
    assert_eq!(
        raws(&browser.levels[TIME]),
        ["CURVED_DAY"],
        "CURVED_NIGHT is not, so that level falls back to its first choice"
    );
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[5])));
}

/// Two wildcards in one name are still one level each: the second lists only
/// what goes with the first, so picking `OVERLAY` cannot leave a `CURVED_*`
/// selected (and unopenable).
#[test]
fn a_wildcard_follows_the_one_before_it() {
    let pack = Pack::new("pairs", SAMPLE);
    let mut browser = pack.browser();

    browser.select(1, 1);
    browser.select(TIME, 1);
    browser.select(TYPE, 1); // OVERLAY

    assert_eq!(raws(&browser.levels[TIME]), ["FLAT_DAY"]);
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[3])));
}

/// A wildcard takes as little as it can, so a name splits at its *first*
/// separator -- except for the last wildcard of a name, which takes whatever is
/// left over.
#[test]
fn wildcards_are_lazy() {
    let segment = Segment::parse("MBZ__<Level>__<Type>.slangp").expect("valid pattern");
    assert_eq!(
        segment.captures("MBZ__0__SMOOTH-ADV__GDV.slangp"),
        Some(vec!["0".to_owned(), "SMOOTH-ADV__GDV".to_owned()])
    );
    assert_eq!(segment.captures("MBZ__0.slangp"), None);

    let segment = Segment::parse("<Type>_<Time>.slangp").expect("valid pattern");
    assert_eq!(
        segment.captures("NEAR_CURVED_NIGHT.slangp"),
        Some(vec!["NEAR".to_owned(), "CURVED_NIGHT".to_owned()])
    );
    assert_eq!(
        segment.captures("PLAIN.slangp"),
        None,
        "a name without the separator is not this shape"
    );

    // What was taken apart goes back together the same way.
    assert_eq!(
        segment.compose(&["NEAR".to_owned(), "CURVED_NIGHT".to_owned()]),
        "NEAR_CURVED_NIGHT.slangp"
    );
}

/// A pattern with one wildcard is one combo box, and everything that is not its
/// shape -- another directory, a file of another kind -- is left out of it.
#[test]
fn a_single_wildcard_is_a_single_level() {
    let pack = Pack::new("flat", &["border/gb-pocket.slangp", "border/gg.slangp"]);
    std::fs::create_dir_all(pack.0.join("border/resources")).expect("mkdir");
    std::fs::write(pack.0.join("border/README.md"), "notes").expect("write");

    let browser = pack.browse("border/<Type>.slangp");
    assert_eq!(browser.names, ["Type"]);
    assert_eq!(raws(&browser.levels[0]), ["gb-pocket", "gg"]);
    assert_eq!(
        browser.path(),
        Some(pack.0.join("border/gb-pocket.slangp"))
    );
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

    // A preset from somewhere else is not this collection's business, and leaves
    // the selection alone.
    let elsewhere = PathBuf::from("shaders/slangp/crt/crt-lottes.slangp");
    assert!(!browser.reveal(&elsewhere));
    assert_eq!(browser.path(), Some(pack.0.join(SAMPLE[4])));

    // Neither is a path of the right shape naming something the collection does
    // not ship -- but the levels that did match are selected.
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
                    for time in 0..browser.levels[TIME].choices.len() {
                        browser.select(TIME, time);
                        let path = browser.path().expect("a full selection has a path");
                        assert!(path.is_file(), "{path:?} does not exist");
                    }
                }
            }
        }
    }
}

/// Nothing to browse is reported rather than opened: a missing tree (the
/// `shaders/` checkout is gitignored, so a fresh clone has none), one whose
/// directories hold no preset of that shape, and a malformed pattern.
#[test]
fn an_unusable_collection_is_an_error() {
    let missing = std::env::temp_dir().join("demarc-shader-dialog-absent");
    let _ = std::fs::remove_dir_all(&missing);
    assert!(PresetBrowser::new(missing, PACK).is_err());

    let empty = Pack::new("empty", &["Machine/Monitor/Flavour/notes.txt"]);
    assert!(PresetBrowser::new(empty.0.clone(), PACK).is_err());

    assert!(PresetBrowser::new(empty.0.clone(), "<Type.slangp").is_err());
}

/// Each matched string is labelled the way it reads: machine and monitor names
/// are words, shouted directory names are codes and stay as they are, and the
/// shouted halves of a file name become words again.
#[test]
fn labels_suit_the_string() {
    assert_eq!(label("Commodore_Amiga500", false), "Commodore Amiga500");
    assert_eq!(label("MBZ_SHARP_STD", false), "MBZ_SHARP_STD");
    assert_eq!(label("NEAR_CURVED", true), "Near Curved");
    assert_eq!(label("NIGHT", true), "Night");
    assert_eq!(label("gb-pocket", true), "gb-pocket");

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
    assert_eq!(labels(&browser.levels[TYPE]), ["Near", "Overlay"]);
}

/// What the dialog prints under the combo boxes: the preset without the
/// collection root, which is the tail of a `--slangp` argument.
#[test]
fn the_shown_path_is_relative_to_the_collection() {
    let pack = Pack::new("relative", SAMPLE);
    let browser = pack.browser();
    assert_eq!(browser.relative_path().as_deref(), Some(SAMPLE[0]));
}

/// One collection per table of the config, in the order it writes them; a
/// pattern nothing matches is left out rather than offered as empty combo
/// boxes.
#[test]
fn collections_come_from_the_config() {
    let pack = Pack::new("config", SAMPLE);
    let config = format!(
        "[Commodore]\npattern = \"{PACK}\"\n\n\
         [Absent]\npattern = \"nowhere/<Type>.slangp\"\n"
    );
    let found = parse_collections(&pack.0, &config);

    assert_eq!(found.len(), 1);
    assert_eq!(found[0].label, "Commodore");
    assert_eq!(
        found[0].browser.as_ref().and_then(PresetBrowser::path),
        Some(pack.0.join(SAMPLE[0]))
    );
}

/// A dialog over one collection, plus the default collection every dialog has.
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
/// what leaves it with no rows.
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

    // A preset of the collection's shape that it no longer ships is still the
    // collection's, so it stays selected rather than falling back to the
    // default.
    dialog.reveal(
        &pack
            .0
            .join("Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/GONE_FLAT_DAY.slangp"),
    );
    assert_eq!(dialog.selected, 1);

    // The built-in shader belongs to no collection, and is the default.
    dialog.reveal(Path::new("shaders/slangp/crt/crt-lottes.slangp"));
    assert_eq!(dialog.selected, DEFAULT);
    assert!(dialog.browser().is_none());
}

/// With no config installed there is still a collection to show: the default,
/// which browses nothing.
#[test]
fn the_default_collection_is_always_there() {
    let found = collections();
    assert_eq!(found[DEFAULT].label, "Default");
    assert!(found[DEFAULT].browser.is_none());
}

/// The real `shaders/shaders.toml`, if this checkout has one. Ignored for the
/// same reason `post_process_tests::megabezel_pack_presets_resolve` is: it needs
/// `shaders/` laid out as `docs/SHADERS.md` describes.
#[test]
#[ignore]
fn the_installed_collections_browse() {
    let mut found = collections();
    assert!(found.len() > 1, "no shaders.toml, or nothing in it matched");

    let pack = found
        .iter_mut()
        .find(|c| c.label == "Commodore")
        .expect("no Commodore collection");
    let browser = pack.browser.as_mut().expect("a collection with a tree");

    let wanted = Path::new("shaders").join(
        "Mega_Bezel_Packs/TheNamec-Commodore/presets/Commodore_Amiga500/Commodore_C1084/MBZ_SHARP_STD/NEAR_CURVED_NIGHT.slangp",
    );
    assert!(browser.reveal(&wanted), "{wanted:?} should be in the pack");
    assert_eq!(
        labels(&browser.levels[0])[browser.levels[0].index],
        "Commodore Amiga500"
    );
    assert_eq!(browser.path(), Some(wanted));
}

/// A dot directory sorts before every name and holds no presets, so a `.git` in
/// a shader checkout must not become the first choice of its level.
#[test]
fn hidden_directories_are_skipped() {
    let pack = Pack::new(
        "hidden",
        &[
            ".git/objects/Monitor/Flavour/NEAR_DAY.slangp",
            "Machine/Monitor/Flavour/NEAR_DAY.slangp",
        ],
    );
    let browser = pack.browser();
    assert_eq!(raws(&browser.levels[0]), ["Machine"]);
}
