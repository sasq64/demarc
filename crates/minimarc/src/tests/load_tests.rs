use super::*;
use database::emu_file::FileSource;
use std::path::Path;

fn root(rel: &str) -> PathBuf {
    Path::new(env!("DEMARC_ROOT")).join(rel)
}

#[test]
fn only_demo_categories_are_read() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csdb.txt");
    std::fs::write(
        &db,
        "# CSDb database\n# Platform:C64\n\
         id:1\ttitle:Embryo\tauthor:Padua\tcategory:Demo\tdownload:https://x.org/a.zip\n\
         id:2\ttitle:Some Game +3\tauthor:Nobody\tcategory:Crack\tdownload:https://x.org/b.zip\n\
         id:3\ttitle:Tiny\tauthor:Padua\tcategory:4K Intro\tdownload:https://x.org/c.zip\n",
    )
    .unwrap();
    let (name, files) = read_db(&db).unwrap();
    assert_eq!(name, "CSDb");
    let titles: Vec<_> = files.iter().map(|f| f.game_info.title).collect();
    assert_eq!(titles, ["Embryo", "Tiny"]);
}

#[test]
fn entries_are_sorted_by_rating() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csdb.txt");
    std::fs::write(
        &db,
        "# CSDb database\n# Platform:C64\n\
         id:1\ttitle:Unrated\tauthor:A\tcategory:Demo\tdownload:https://x.org/a.zip\n\
         id:2\ttitle:Good\tauthor:A\tcategory:Demo\trating:8.5\tdownload:https://x.org/b.zip\n\
         id:3\ttitle:Best\tauthor:A\tcategory:Demo\trating:9.7\tdownload:https://x.org/c.zip\n",
    )
    .unwrap();
    let (_, files) = read_db(&db).unwrap();
    let titles: Vec<_> = files.iter().map(|f| f.game_info.title).collect();
    assert_eq!(titles, ["Best", "Good", "Unrated"]);
}

#[test]
fn demos_come_first_among_equal_ratings() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csdb.txt");
    std::fs::write(
        &db,
        "# CSDb database\n# Platform:C64\n\
         id:1\ttitle:Tune\tauthor:A\tcategory:Music\trating:9.0\tdownload:https://x.org/a.zip\n\
         id:2\ttitle:Onefiler\tauthor:A\tcategory:One-File Demo\trating:9.0\tdownload:https://x.org/b.zip\n\
         id:3\ttitle:Pic\tauthor:A\tcategory:Graphics\trating:9.5\tdownload:https://x.org/c.zip\n\
         id:4\ttitle:Big\tauthor:A\tcategory:Demo\trating:9.0\tdownload:https://x.org/d.zip\n",
    )
    .unwrap();
    let (_, files) = read_db(&db).unwrap();
    let titles: Vec<_> = files.iter().map(|f| f.game_info.title).collect();
    assert_eq!(titles, ["Pic", "Onefiler", "Big", "Tune"]);
}

#[test]
fn a_zipped_disk_set_is_prepared_for_vice() {
    let file = EmuFile {
        path: FileSource::Path(root("testdata/c64/Skaaneland.zip")),
        ..Default::default()
    };
    let demo = prepare(file).unwrap();
    assert_eq!(demo.work.path.extension().unwrap(), "m3u");
    assert_eq!(
        demo.work.get_meta_or("vice_cartridge", ""),
        "rr38ppal-auto.crt"
    );
    assert_eq!(demo.events, release_io::c64::FAST_LOAD_EVENTS);
    assert_eq!(demo.info, "");
}

#[test]
fn an_override_sets_meta_and_adds_its_keys() {
    use retro_core::libretro::RETROK_SPACE;

    let file = EmuFile {
        path: FileSource::Path(root("testdata/c64/Skaaneland.zip")),
        ..Default::default()
    };
    let over = Override {
        meta: [("vice_sid_extra", "0xd420"), ("c64_fast_load", "disabled")].into(),
        events: vec![(300, InputEvent::Key(RETROK_SPACE))],
        ..Default::default()
    };
    let demo = prepare_with(file, Some(&over)).unwrap();
    assert_eq!(demo.work.get_meta_or("vice_sid_extra", ""), "0xd420");
    assert!(!demo.work.has_meta("vice_cartridge"));
    assert_eq!(demo.events, [(300, InputEvent::Key(RETROK_SPACE))]);
}

#[test]
fn a_db_entry_is_keyed_for_overrides_and_described() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("csdb.txt");
    std::fs::write(
        &db,
        "# CSDb database\n# Platform:C64\n\
         id:197970\ttitle:Embryo\tauthor:Padua\tcategory:Demo\tdate:2020-10-31\t\
         download:https://x.org/a.zip\n",
    )
    .unwrap();
    let (_, files) = read_db(&db).unwrap();
    assert_eq!(files[0].demo_id(), DemoId::new("csdb", 197970));
    assert_eq!(info_text(&files[0]), "\"Embryo\"\nPadua (2020)\nC64 Demo");
}

#[test]
fn a_release_with_nothing_for_a_c64_is_an_error() {
    let file = EmuFile {
        path: FileSource::Path(root("testdata/demos.txt")),
        ..Default::default()
    };
    assert!(prepare(file).is_err());
}
