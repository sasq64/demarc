use crate::emu_file::{Download, release_downloads};

use super::*;

/// The rank is the third item of the `pouet` field. A release that is on
/// pouet but unranked leaves that item empty, and one that isn't there at
/// all has no field, so neither gets a rank to sort on.
#[test]
fn pouet_rank_comes_from_the_third_item() {
    assert_eq!(parse_pouet_rank("17,828,1,8,1 5 11"), Some(1));
    assert_eq!(parse_pouet_rank("0,146,382,,"), Some(382));
    assert_eq!(parse_pouet_rank("0,1,,,"), None);
    assert_eq!(parse_pouet_rank("0,1"), None);
    assert_eq!(parse_pouet_rank(""), None);
}

/// A db line's `pouet` field ranks the entry; a line without one doesn't.
#[test]
fn collect_db_reads_the_pouet_rank() {
    let mut out = vec![];
    collect_db_text(
        "title:Ranked\tdownload:http://example.com/a.zip\tpouet:0,146,382,,\n\
         title:Unranked\tdownload:http://example.com/b.zip\n",
        &DbFilter::default(),
        &mut out,
    );
    assert_eq!(out[0].game_info.rank, 382);
    assert_eq!(out[1].game_info.rank, 0);
}

/// An Amiga release is one entry, not one per file: the directory is
/// mounted as the hard drive it boots from, so the executable's data files
/// have to come with it. `--many` still asks for the files on their own.
#[test]
#[ignore] // TODO: Maybe remove, we are relaxing file collection logic
fn amiga_release_directory_is_collected_whole() {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("eph-fels");
    fs::create_dir_all(release.join("musikk")).unwrap();
    fs::write(release.join("fels.exe"), [0x00, 0x00, 0x03, 0xF3]).unwrap();
    fs::write(release.join("fels.readme"), b"a readme").unwrap();
    fs::write(release.join("musikk").join("tune.p61"), b"a tune").unwrap();

    let mut out = vec![];
    collect_files(dir.path(), &mut out, false).unwrap();
    assert_eq!(out.len(), 1);
    assert!(matches!(&out[0].path, FileSource::Path(p) if *p == release));

    let mut out = vec![];
    collect_files(dir.path(), &mut out, true).unwrap();
    assert!(out.len() > 1, "--many splits the release up again");
}

#[test]
fn missing_directory_is_an_error() {
    let mut out = vec![];
    assert!(collect_files(Path::new("no/such/dir"), &mut out, false).is_err());
    assert!(out.is_empty());
}

/// Every field spells out its name, so order doesn't matter and a value may
/// hold `:` itself (every URL does). The fields become meta, with the title,
/// group and year lifted into the entry's `GameInfo`. Blank lines and
/// URL-less lines are skipped.
#[test]
fn collect_db_parses_named_fields() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("demos.txt");
    fs::write(
        &db,
        "id:1\ttitle:Zentro 4\tauthor:Zenith\tdate:1992-12-27\tparty:The Party 1992\tcategory:Demo\ttags:has effects\tdownload:http://example.com/zentro4;http://example.com/zentro4.dms\n\
         \n\
         download:https://example.com/nexus7.zip\tauthor:Andromeda\ttitle:Nexus 7\tdate:1994/12/30\n\
         id:3\ttitle:No URL\tauthor:Group\tdate:1994\tparty:\tcategory:Intro\ttags:\tdownload:\n\
         id:4\ttitle:Musicdisk\tauthor:Group\tdate:1992\tparty:\tcategory:Musicdisk\ttags:\tdownload:http://example.com/md.dms\n",
    )
    .unwrap();

    let mut out = vec![];
    collect_db(&db, &DbFilter::default(), &mut out).unwrap();
    assert_eq!(out.len(), 3, "blank, URL-less and disk lines skipped");

    let zentro = &out[0];
    let FileSource::Url(urls) = &zentro.path else {
        panic!("db entries stay URLs until loaded, got {:?}", zentro.path)
    };
    assert_eq!(
        urls.as_slice(),
        [
            "http://example.com/zentro4",
            "http://example.com/zentro4.dms"
        ]
    );
    assert_eq!(zentro.game_info.title, "Zentro 4");
    assert_eq!(zentro.game_info.group, "Zenith");
    assert_eq!(zentro.game_info.year(), 1992);
    assert_eq!(zentro.get_meta("category"), "Demo");
    assert_eq!(zentro.get_meta("party"), "The Party 1992");
    assert_eq!(zentro.get_meta("tags"), "has effects");

    // Fields in any order, missing ones simply left empty.
    let nexus = &out[1];
    assert_eq!(nexus.game_info.title, "Nexus 7");
    assert_eq!(nexus.game_info.group, "Andromeda");
    assert_eq!(nexus.game_info.year(), 1994);
    assert!(!nexus.meta.contains_key("party"));
}

/// A packed db loads exactly like the plain text one it was made from — the
/// dbs are big and ship compressed, so both gzip and bzip2 are unpacked on
/// the way in.
#[test]
fn collect_db_reads_packed_dbs() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for packed in ["testdata/demos.txt.gz", "testdata/demos.txt.bz2"] {
        let mut out = vec![];
        collect_db(&root.join(packed), &DbFilter::default(), &mut out).unwrap();
        assert_eq!(out.len(), 2, "{packed}");

        let eod = &out[0];
        assert_eq!(eod.game_info.title, "Edge of Disgrace", "{packed}");
        assert_eq!(eod.game_info.group, "Booze Design", "{packed}");
        assert_eq!(eod.game_info.year(), 2008, "{packed}");
        // The `# Platform:C64` header applies just as it does unpacked.
        assert_eq!(eod.get_meta("platform"), "C64", "{packed}");
        assert_eq!(eod.get_meta("category"), "demo", "{packed}");
        let FileSource::Url(urls) = &eod.path else {
            panic!("db entries stay URLs until loaded, got {:?}", eod.path)
        };
        assert_eq!(urls.as_slice(), ["https://example.com/eod.d64"], "{packed}");
        assert_eq!(out[1].game_info.title, "Nexus 7", "{packed}");
    }
}

/// A db piped in has usually been filtered line by line, so the header that
/// carried the platform may be gone and only some lines survive — each line
/// still stands on its own.
#[test]
fn collect_db_parses_filtered_lines() {
    let mut out = vec![];
    collect_db_text(
        "id:9\ttitle:Speedball Demo\tauthor:Illusions\tdate:1990-04-07\tcategory:Demo\tdownload:http://example.com/speedball\n",
        &DbFilter::default(),
        &mut out,
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].game_info.title, "Speedball Demo");
    assert_eq!(out[0].get_meta("category"), "Demo");
    assert!(
        !out[0].meta.contains_key("platform"),
        "no header, no platform"
    );
}

/// `--include`/`--exclude` drop non-matching lines while collecting, but
/// header comments are still read so the platform they set reaches the
/// survivors.
#[test]
fn collect_db_applies_filter() {
    const DB: &str = "# Platform:Amiga\n\
         id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
         id:2\ttitle:Musicdisk\tcategory:Musicdisk\tdownload:http://example.com/md.dms\n\
         id:3\ttitle:Nexus 7\tcategory:Demo\ttags:aga\tdownload:http://example.com/nexus7.zip\n";

    let titles = |filter: &DbFilter| {
        let mut out = vec![];
        collect_db_text(DB, filter, &mut out);
        (
            out.iter().map(|f| f.game_info.title).collect::<Vec<_>>(),
            out,
        )
    };

    let re = |p: &str| [Regex::new(p).unwrap()];

    let include = re("category:Demo");
    let (kept, out) = titles(&DbFilter {
        include: &include,
        ..Default::default()
    });
    assert_eq!(kept, ["Zentro 4", "Nexus 7"]);
    assert_eq!(out[0].get_meta("platform"), "Amiga", "header still applies");

    let exclude = re("category:Musicdisk");
    let (kept, _) = titles(&DbFilter {
        exclude: &exclude,
        ..Default::default()
    });
    assert_eq!(kept, ["Zentro 4", "Nexus 7"]);

    // Both apply: a line has to match `include` and miss `exclude`.
    let exclude = re("tags:aga");
    let (kept, _) = titles(&DbFilter {
        include: &include,
        exclude: &exclude,
    });
    assert_eq!(kept, ["Zentro 4"]);

    // Several includes are AND:ed — only the line matching both survives.
    let include = [
        Regex::new("category:Demo").unwrap(),
        Regex::new("aga").unwrap(),
    ];
    let (kept, _) = titles(&DbFilter {
        include: &include,
        ..Default::default()
    });
    assert_eq!(kept, ["Nexus 7"]);

    // Several excludes are OR:ed — matching either one drops the line.
    let exclude = [
        Regex::new("category:Musicdisk").unwrap(),
        Regex::new("tags:aga").unwrap(),
    ];
    let (kept, _) = titles(&DbFilter {
        exclude: &exclude,
        ..Default::default()
    });
    assert_eq!(kept, ["Zentro 4"]);
}

/// Each pattern is matched against one field at a time, so it can't run
/// past the end of the field it names into the next one, and `^`/`$`
/// anchor to a field rather than the whole line.
#[test]
fn collect_db_filter_matches_per_field() {
    const DB: &str = "id:1\ttitle:Firefox Intro\tauthor:Zenith\tcategory:Demo\tdownload:http://example.com/a\n\
         id:2\ttitle:Hoax\tauthor:Firefox\tcategory:Demoshow\tdownload:http://example.com/b\n";

    let titles = |filter: &DbFilter| {
        let mut out = vec![];
        collect_db_text(DB, filter, &mut out);
        out.iter().map(|f| f.game_info.title).collect::<Vec<_>>()
    };

    // `.*` stops at the field end, so the `Firefox` in the title of the
    // first line doesn't count as an author.
    let include = [Regex::new("author:.*Firefox").unwrap()];
    assert_eq!(
        titles(&DbFilter {
            include: &include,
            ..Default::default()
        }),
        ["Hoax"]
    );

    // `$` is the end of a field, so `Demoshow` isn't a `Demo`.
    let include = [Regex::new("^category:Demo$").unwrap()];
    assert_eq!(
        titles(&DbFilter {
            include: &include,
            ..Default::default()
        }),
        ["Firefox Intro"]
    );

    // A pattern spanning the tab between two fields matches nothing.
    let include = [Regex::new("title:Hoax\tauthor:Firefox").unwrap()];
    assert!(
        titles(&DbFilter {
            include: &include,
            ..Default::default()
        })
        .is_empty()
    );
}

/// A `# Platform:` header covers the lines below it, giving each one a
/// `platform` tag, and a line naming a platform of its own overrides it —
/// that way entries from different scenes stay apart.
#[test]
fn collect_db_platform_tags_lines() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("demos.txt");
    fs::write(
        &db,
        "# Platform:Amiga\n\
         id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
         id:2\ttitle:Embryo\tcategory:Demo\tplatform:C64\tdownload:http://example.com/embryo.zip\n",
    )
    .unwrap();

    let mut out = vec![];
    collect_db(&db, &DbFilter::default(), &mut out).unwrap();
    assert_eq!(out[0].get_meta("platform"), "Amiga", "header applies");
    assert_eq!(out[0].get_meta("category"), "Demo");
    assert_eq!(out[1].get_meta("platform"), "C64", "line overrides header");
}

/// The other pairs of a header line become meta on every entry below it,
/// while a plain prose comment is left alone.
#[test]
fn collect_db_applies_header_tags() {
    let mut out = vec![];
    collect_db_text(
        "# Platform:Amiga puae_model:A500\n\
         # Just a comment: nothing to see here\n\
         id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
         # puae_model:A1200\n\
         id:2\ttitle:Nexus 7\tcategory:Demo\ttags:aga\tdownload:http://example.com/nexus7.zip\n",
        &DbFilter::default(),
        &mut out,
    );

    assert_eq!(out[0].get_meta("platform"), "Amiga");
    assert_eq!(out[0].get_meta("puae_model"), "A500");
    assert!(!out[0].meta.contains_key("Just"));
    assert!(!out[0].meta.contains_key("comment"));

    // A later header overrides, and the platform from the first one still
    // applies.
    assert_eq!(out[1].get_meta("puae_model"), "A1200");
    assert_eq!(out[1].get_meta("tags"), "aga");
    assert_eq!(out[1].get_meta("platform"), "Amiga");
}

/// A disk image among the URLs makes the release disk based: the disk set
/// is the first thing to try, whatever format its disks are in, and the
/// extras are gone. Anything else that could be the release stays on as a
/// fallback for when the disk links are dead.
#[test]
fn disk_images_win() {
    assert_eq!(
        release_downloads(&[
            "https://x.com/a.pdf",
            "https://x.com/a1.d64",
            "https://x.com/a2.D64",
            "https://x.com/a.adf",
            "https://x.com/a.zip",
            "https://x.com/readme.txt",
        ]),
        vec![
            Download::Disks(vec![
                vec!["https://x.com/a1.d64"],
                vec!["https://x.com/a2.D64"],
                vec!["https://x.com/a.adf"],
            ]),
            Download::File("https://x.com/a.zip"),
            Download::File("https://x.com/readme.txt"),
        ]
    );
}

/// Two disk images of the same name are one disk in two formats, not two
/// disks — demozoo lists D.O.S. by Andromeda exactly like this — so they
/// become each other's fallback instead of a set that only half exists.
#[test]
fn same_named_images_are_one_disk() {
    assert_eq!(
        release_downloads(&[
            "AmigascneFile:/Groups/A/Andromeda/Andromeda-dos.adf",
            "AmigascneFile:/Groups/A/Andromeda/ANDROMEDA-DOS.dms",
            "SceneOrgFile:/parties/1992/thegathering92/amiga_demo/andromeda-d_o_s.zip",
        ]),
        vec![
            Download::Disks(vec![vec![
                "AmigascneFile:/Groups/A/Andromeda/Andromeda-dos.adf",
                "AmigascneFile:/Groups/A/Andromeda/ANDROMEDA-DOS.dms",
            ]]),
            Download::File(
                "SceneOrgFile:/parties/1992/thegathering92/amiga_demo/andromeda-d_o_s.zip"
            ),
        ]
    );
}

/// Without a disk image every URL is an attempt of its own, in the order
/// the release lists them; only the known extras are dropped, and
/// extension-less URLs are left for the loader to sort out.
#[test]
fn extras_dropped() {
    assert_eq!(
        release_downloads(&[
            "https://x.com/demo.sid",
            "https://x.com/demo.zip",
            "https://x.com/scan.PDF",
            "https://x.com/download.php?id=1",
        ]),
        vec![
            Download::File("https://x.com/demo.zip"),
            Download::File("https://x.com/download.php?id=1"),
        ]
    );
}

/// Filtering everything away would leave nothing to fetch, so the input
/// survives as attempts instead.
#[test]
fn all_filtered_keeps_input() {
    assert_eq!(
        release_downloads(&["https://x.com/a.sid", "https://x.com/b.pdf"]),
        vec![
            Download::File("https://x.com/a.sid"),
            Download::File("https://x.com/b.pdf"),
        ]
    );
}
