use super::*;

/// The file as it is actually written: which download to take, which
/// program inside it to start, core options and a patch.
#[test]
fn parses_an_override_per_release() {
    let overrides = parse(
        r#"
        [zoo.102]
        file = "rgba_tbc_elevated.zip"
        boot = "elevated_1280x720.exe"

        [zoo.68604]
        libretro = { dosbox_pure_cycles = "max" }

        [zoo.57849]
        libretro = { dosbox_pure_cycles = 150000 }
        meta = { dos4gw = true }

        [zoo.18030]
        file = "inside.zip"
        patch = { info = "GUS 0x240", target = "SOUND.CFG", contents = "AAEC" }
        "#,
    )
    .unwrap();
    assert_eq!(overrides.len(), 4);

    let elevated = &overrides[&102];
    assert_eq!(elevated.download, Some("rgba_tbc_elevated.zip"));
    assert_eq!(elevated.boot_file, Some("elevated_1280x720.exe"));
    assert!(elevated.patches.is_empty());

    assert_eq!(overrides[&68604].meta["dosbox_pure_cycles"], "max");
    // A number written unquoted is still a meta value, as is a bool.
    assert_eq!(overrides[&57849].meta["dosbox_pure_cycles"], "150000");
    assert_eq!(overrides[&57849].meta["dos4gw"], "true");

    let inside = &overrides[&18030];
    assert_eq!(inside.patches.len(), 1);
    assert_eq!(inside.patches[0].target, "SOUND.CFG");
    assert_eq!(inside.patches[0].info, "GUS 0x240");
    assert_eq!(inside.patches[0].offset, None);
    assert_eq!(inside.patches[0].bytes().unwrap(), [0, 1, 2]);
}

/// `assign` is written as a table of AmigaDOS names, and arrives as the one
/// `assign` meta string `newsys::amiga` splits back apart.
#[test]
fn folds_assigns_into_one_meta_value() {
    let overrides = parse(
        r#"
        [zoo.119665]
        assign = { Love = "SYS:" }

        [zoo.2]
        assign = { Data = "DH0:data", Music = "DH0:mod" }
        "#,
    )
    .unwrap();
    assert_eq!(overrides[&119665].meta["assign"], "Love=SYS:");
    assert_eq!(overrides[&2].meta["assign"], "Data=DH0:data;Music=DH0:mod");
    // Nothing written, nothing set — the Amiga side never sees the key.
    assert!(
        !parse("[zoo.3]\nfile = \"a.zip\"\n").unwrap()[&3]
            .meta
            .contains_key("assign")
    );
}

/// `fast = true` is one word standing in for a whole Amiga configuration,
/// and is applied before the entry's own options so those still win.
#[test]
fn takes_the_fast_amiga_configuration() {
    let overrides = parse(
        r#"
        [zoo.7236]
        fast = true

        [zoo.108]
        file = "2nd_real.zip"
        "#,
    )
    .unwrap();
    assert!(overrides[&7236].fast);
    assert!(!overrides[&108].fast);
}

/// A release needing more than one file written gets an array of patches,
/// and a patch may write into a file rather than replace it.
#[test]
fn parses_several_patches() {
    let overrides = parse(
        r#"
        [[zoo.1.patch]]
        target = "SOUND.CFG"
        contents = "AAEC"

        [[zoo.1.patch]]
        target = "DEMO.EXE"
        offset = 1024
        contents = "AAEC"
        "#,
    )
    .unwrap();
    let patches = &overrides[&1].patches;
    assert_eq!(patches.len(), 2);
    assert_eq!(patches[0].offset, None);
    assert_eq!(patches[1].offset, Some(1024));
}

/// One unusable entry is dropped on its own — the rest of the file is
/// still worth having.
#[test]
fn drops_only_the_bad_entry() {
    let overrides = parse(
        r#"
        [zoo.not-an-id]
        file = "a.zip"

        [zoo.2]
        patch = { target = "A.CFG", contents = "not base64!" }

        [zoo.3]
        file = "c.zip"
        "#,
    )
    .unwrap();
    assert_eq!(overrides.len(), 1);
    assert_eq!(overrides[&3].download, Some("c.zip"));
}

/// A section outside `zoo` is a typo rather than a feature, so it is
/// ignored — while a misspelled *key* inside an entry is an error, since
/// there is nowhere else it could have been meant to go.
#[test]
fn rejects_what_it_cannot_apply() {
    assert!(parse("[zoo_57849]\nfile = \"a.zip\"\n").unwrap().is_empty());
    assert!(parse("[zoo.1]\nfil = \"a.zip\"\n").is_err());
    assert!(parse("[zoo.1] file =").is_err());
}
