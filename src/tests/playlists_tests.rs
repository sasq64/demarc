use super::*;
use crate::files::{DbFilter, collect_db_text};

fn entry(line: &'static str) -> EmuFile {
    let mut files = vec![];
    collect_db_text(line, &DbFilter::default(), &mut files);
    let mut file = files.remove(0);
    file.meta.insert("db", "Demozoo");
    file
}

#[test]
fn picker_ticks_members_and_offers_new_lists() {
    let dir = tempfile::tempdir().unwrap();
    let file = entry("id:7\ttitle:A\tdownload:http://x/a.zip\n");
    let mut lists = Playlists::load(dir.path());
    lists.toggle(0, &file, "");
    let broken = lists.create("Broken").unwrap();

    let picker = PlaylistPicker::new(&lists, &file);
    let source: &dyn FuzzySource<EmuFile> = &picker;
    assert_eq!(source.search("", 10), [0, broken]);
    assert!(source.get_text(0).starts_with("Favorites "));
    assert_eq!(source.get_text(broken), "Broken");
    assert_eq!(source.search("broken", 10), [broken]);
    assert_eq!(source.search("bro", 10), [broken, 2]);
    assert_eq!(source.get_text(2), "+ New \"bro\"");
    assert_eq!(picker.query(), "bro");
}
