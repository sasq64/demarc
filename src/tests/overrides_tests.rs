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

#[test]
fn patch_from_system_dir_source() {
    let overrides = parse(
        r#"
        [zoo.1]
        patch = { target = "overrides.toml", source = "overrides.toml" }

        [zoo.2]
        patch = { target = "A.DLL", source = "no/such/file.dll" }

        [zoo.3]
        patch = { target = "A.CFG", contents = "AAEC", source = "overrides.toml" }
        "#,
    )
    .unwrap();
    assert_eq!(overrides.len(), 1);
    let patch = &overrides[&1].patches[0];
    assert_eq!(patch.source, Some("overrides.toml"));
    assert!(!patch.bytes().unwrap().is_empty());
}

/// `events` names keys the way remote scripts do, and arrives as retro keycodes.
#[test]
fn parses_key_events() {
    let overrides = parse(
        r#"
        [zoo.108]
        events = [{ frame = 50, key = "Enter" }, { frame = 60, key = "KeyA" }, { frame = 70, key = "1" }, { frame = 80, key = "b" }]

        [zoo.2]
        events = [{ frame = 1, key = "NoSuchKey" }]
        "#,
    )
    .unwrap();
    assert_eq!(
        overrides[&108].events,
        [
            (50, crate::libretro::RETROK_RETURN),
            (60, crate::libretro::RETROK_a),
            (70, crate::libretro::RETROK_1),
            (80, crate::libretro::RETROK_b)
        ]
    );
    assert!(!overrides.contains_key(&2));
}

/// `download` replaces the release's own links, and has to be a URL.
#[test]
fn download_overrides_the_url() {
    let overrides = parse(
        r#"
        [zoo.311767]
        download = "https://example.org/area5150_86box.zip"
        "#,
    )
    .unwrap();
    assert_eq!(
        overrides[&311767].download_url,
        Some("https://example.org/area5150_86box.zip")
    );

    let overrides = parse(
        r#"
        [zoo.1]
        download = "area5150.zip"
        "#,
    )
    .unwrap();
    assert!(overrides.is_empty());
}

/// A bsdiff patch is written as `format = "bsdiff"` and the delta under
/// `patch`, and is checked at startup like any other patch data.
#[test]
fn parses_a_bsdiff_patch() {
    let delta = bsdiff(b"old contents", b"new contents");
    let overrides = parse(&format!(
        r#"
        [zoo.301363]
        patch = {{ target = "demo.dat", format = "bsdiff", patch = """
{delta}
""" }}
        "#
    ))
    .unwrap();
    let patch = &overrides[&301363].patches[0];
    assert!(patch.bsdiff);
    assert_eq!(patch.target, "demo.dat");
    // Wrapped over several lines in the file, and still the delta it was.
    assert!(patch.data.contains('\n') && patch.bytes().unwrap().starts_with(b"BSDIFF40"));

    // Not a delta at all, an unknown format, and an offset that means nothing
    // for a delta — each one drops its entry.
    for entry in [
        r#"patch = { target = "a", format = "bsdiff", patch = "AAEC" }"#.to_string(),
        format!(r#"patch = {{ target = "a", format = "xdelta", patch = """{delta}""" }}"#),
        format!(
            r#"patch = {{ target = "a", format = "bsdiff", offset = 4, patch = """{delta}""" }}"#
        ),
    ] {
        assert!(
            parse(&format!("[zoo.1]\n{entry}\n")).unwrap().is_empty(),
            "{entry}"
        );
    }
}

/// A bsdiff delta of `source` to `target`, base64 and line wrapped the way an
/// override writes one.
fn bsdiff(source: &[u8], target: &[u8]) -> String {
    use base64::Engine;
    let mut patch = Vec::new();
    qbsdiff::Bsdiff::new(source, target)
        .compare(std::io::Cursor::new(&mut patch))
        .unwrap();
    let text = base64::engine::general_purpose::STANDARD.encode(&patch);
    text.as_bytes()
        .chunks(76)
        .map(|line| String::from_utf8_lossy(line).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}
