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
fn entry_round_trips_through_json() {
    let file = entry("id:42\ttitle:Zentro\tauthor:Zenith\tdate:1992-04-01\tdownload:http://x/z.zip\n");
    let json = serde_json::to_string(&PlaylistEntry::new(&file)).unwrap();
    let back = serde_json::from_str::<PlaylistEntry>(&json).unwrap().to_emu_file();
    assert_eq!(back.game_info.title, "Zentro");
    assert_eq!(back.game_info.group, "Zenith");
    assert_eq!(back.game_info.year(), 1992);
    assert_eq!(back.demo_id(), file.demo_id());
    assert_eq!(format!("{:?}", back.path), format!("{:?}", file.path));
    assert_eq!(entry_id(&back), "zoo:42");
}

#[test]
fn toggle_adds_saves_and_removes() {
    let dir = tempfile::tempdir().unwrap();
    let file = entry("id:7\ttitle:A\tdownload:http://x/a.zip\n");

    let mut lists = Playlists::load(dir.path());
    assert!(!lists.is_favorite(&file));
    assert!(lists.toggle_favorite(&file));
    assert!(lists.is_favorite(&file));

    let reloaded = Playlists::load(dir.path());
    assert!(reloaded.is_favorite(&file));
    assert_eq!(reloaded.favorites().files.len(), 1);

    let mut lists = reloaded;
    assert!(!lists.toggle_favorite(&file));
    assert!(!Playlists::load(dir.path()).is_favorite(&file));
}

#[test]
fn every_json_is_a_playlist_with_favorites_first() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Party.json"), "[]").unwrap();
    fs::write(dir.path().join("Amiga.json"), "[]").unwrap();
    let names: Vec<_> = Playlists::load(dir.path())
        .lists
        .iter()
        .map(|l| l.name.clone())
        .collect();
    assert_eq!(names, ["Favorites", "Amiga", "Party"]);
}

#[test]
fn create_adds_a_sorted_list_that_toggles_on_its_own() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Party.json"), "[]").unwrap();
    let file = entry("id:7\ttitle:A\tdownload:http://x/a.zip\n");

    let mut lists = Playlists::load(dir.path());
    let broken = lists.create(" Broken ").unwrap();
    assert_eq!(broken, 1);
    assert_eq!(lists.create("broken"), Some(broken));
    assert_eq!(lists.create("../x"), None);
    assert!(lists.toggle(broken, &file));
    assert!(!lists.is_favorite(&file));

    let reloaded = Playlists::load(dir.path());
    let names: Vec<_> = reloaded.lists.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Favorites", "Broken", "Party"]);
    assert!(reloaded.lists[1].contains(&file));
}

#[test]
fn picker_ticks_members_and_offers_new_lists() {
    let dir = tempfile::tempdir().unwrap();
    let file = entry("id:7\ttitle:A\tdownload:http://x/a.zip\n");
    let mut lists = Playlists::load(dir.path());
    lists.toggle_favorite(&file);
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
