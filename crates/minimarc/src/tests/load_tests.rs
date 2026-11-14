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
fn a_zipped_disk_set_is_prepared_for_vice() {
    let file = EmuFile {
        path: FileSource::Path(root("testdata/c64/Skaaneland.zip")),
        ..Default::default()
    };
    let work = prepare(file).unwrap();
    assert_eq!(work.path.extension().unwrap(), "m3u");
    assert_eq!(work.get_meta_or("vice_cartridge", ""), "rr38ppal-auto.crt");
}

#[test]
fn a_release_with_nothing_for_a_c64_is_an_error() {
    let file = EmuFile {
        path: FileSource::Path(root("testdata/demos.txt")),
        ..Default::default()
    };
    assert!(prepare(file).is_err());
}
