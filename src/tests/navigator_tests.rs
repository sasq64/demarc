use crate::{
    emu_file::EmuFile,
    files::{DbFilter, collect_db},
    navigator::{Navigator, setup_navigator},
};
use indexmap::IndexMap;
use std::path::PathBuf;

#[test]
fn test_navigator() {
    let filter = DbFilter::default();
    let mut files = vec![];
    let path: PathBuf = "testdata/demos.txt".into();
    let name = collect_db(&path, &filter, &mut files).unwrap();
    assert_eq!(name, "Demozoo");
    let files: &'static [EmuFile] = files.leak();
    let dbs: IndexMap<String, &'static [EmuFile]> = [(name, files)].into();
    let mut navigator = Navigator::new();
    setup_navigator(&dbs, &mut navigator).unwrap();

    navigator.goto("");
    println!(">>Root");
    for line in navigator.get_showing() {
        println!("{line}");
    }
    println!(">>Demozoo");
    navigator.goto("Demozoo");
    for line in navigator.get_showing() {
        println!("{line}");
    }
    println!(">>Demozoo/Parties");
    navigator.goto("Demozoo/Parties");
    for line in navigator.get_showing() {
        println!("{line}");
    }
    println!(">>Compos");
    navigator.goto("Demozoo/Parties/Evoke 2005");
    for line in navigator.get_showing() {
        println!("{line}");
    }
    println!(">>Demo");
    navigator.goto("Demozoo/Parties/Evoke 2005/Demo");
    for line in navigator.get_showing() {
        println!("{line}");
    }
}

fn db(text: &'static str) -> &'static [EmuFile] {
    let mut files = vec![];
    crate::files::collect_db_text(text, &DbFilter::default(), &mut files);
    files.leak()
}

/// Every db registers under its own name and the root lists them in the
/// order they were given, each one navigable on its own.
#[test]
fn every_db_gets_its_own_root_entry() {
    let dbs: IndexMap<String, &'static [EmuFile]> = [
        (
            "Demozoo".to_string(),
            db("id:1\ttitle:A\tplatform:Amiga\tdownload:http://x/a.zip\n"),
        ),
        (
            "CSDb".to_string(),
            db("id:2\ttitle:B\tplatform:C64\tdownload:http://x/b.zip\n\
                id:3\ttitle:C\tplatform:C64\tdownload:http://x/c.zip\n"),
        ),
    ]
    .into();
    let mut navigator = Navigator::new();
    setup_navigator(&dbs, &mut navigator).unwrap();

    navigator.goto("");
    assert_eq!(navigator.get_showing(), vec!["Demozoo", "CSDb"]);

    navigator.goto("CSDb/Platforms");
    assert_eq!(navigator.get_showing(), vec!["C64"]);

    navigator.goto("Demozoo/Platforms");
    assert_eq!(navigator.get_showing(), vec!["Amiga"]);
}

#[test]
fn playlist_shows_its_entries_directly() {
    let mut navigator = Navigator::new();
    navigator.add_playlist(
        "Favorites",
        db("id:1\ttitle:A\tauthor:G\tdownload:http://x/a.zip\n"),
    );
    setup_navigator(&IndexMap::new(), &mut navigator).unwrap();

    navigator.goto("");
    assert_eq!(navigator.get_showing(), vec!["Favorites"]);
    navigator.goto("Favorites");
    assert_eq!(navigator.get_showing(), vec!["A / G"]);
}

#[test]
fn playlist_menu_and_refresh() {
    let mut navigator = Navigator::new();
    navigator.add_playlist(
        "Broken",
        db("id:1\ttitle:A\tauthor:G\tdownload:http://x/a.zip\n"),
    );
    setup_navigator(&IndexMap::new(), &mut navigator).unwrap();
    navigator.go_root();
    navigator.goto("Broken");
    navigator.open_playlist_menu(0);
    assert_eq!(navigator.current_path(), ["Broken", "0"]);
    assert_eq!(navigator.get_showing()[1], "Run...");
    navigator.enter(&["dls"]);
    assert_eq!(navigator.get_showing(), vec!["a.zip"]);

    navigator.back().back();
    navigator.add_playlist("Broken", &[]);
    assert!(navigator.get_showing().is_empty());
    navigator.add_playlist("New", &[]);
    navigator.back();
    assert_eq!(navigator.get_showing(), vec!["Broken", "New"]);
}

#[test]
fn playlist_root_menu_deletes() {
    let mut navigator = Navigator::new();
    navigator.add_playlist("Old", &[]);
    navigator.add_playlist("Keep", &[]);
    setup_navigator(&IndexMap::new(), &mut navigator).unwrap();
    navigator.go_root();
    navigator.open_playlist_root_menu("Old");
    assert_eq!(navigator.current_path(), ["Old"]);
    navigator.remove_playlist("Old");
    navigator.pop();
    assert_eq!(navigator.get_showing(), vec!["Keep"]);
}

#[test]
fn files_at_lists_entries_without_navigating() {
    let dbs: IndexMap<String, &'static [EmuFile]> = [(
        "Demozoo".to_string(),
        db("id:1\ttitle:A\tplatform:Amiga\tdownload:http://x/a.zip\n\
            id:2\ttitle:B\tplatform:C64\tdownload:http://x/b.zip\n"),
    )]
    .into();
    let mut navigator = Navigator::new();
    setup_navigator(&dbs, &mut navigator).unwrap();
    navigator.go_root();

    let files = navigator.files_at("Demozoo/Platforms/C64").unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].get_meta("id"), "2");
    assert!(navigator.files_at("Demozoo/Platforms").unwrap().is_empty());
    assert!(navigator.files_at("Nope/Platforms").is_none());
    assert!(navigator.current_path().is_empty());
}

#[test]
fn awards_by_event_and_category() {
    let dbs: IndexMap<String, &'static [EmuFile]> = [(
        "Demozoo".to_string(),
        db("id:1\ttitle:A\tauthor:G\tdate:2007-05\tpouet:0,0,0,8,9\tdownload:http://x/a.zip\n\
            id:2\ttitle:B\tauthor:H\tdate:2024-01\tpouet:0,0,0,,19\tdownload:http://x/b.zip\n\
            id:3\ttitle:C\tauthor:I\tdate:2024-02\tpouet:0,0,0,19,\tdownload:http://x/c.zip\n\
            id:4\ttitle:D\tauthor:J\tparty:Revision 2024;Demo;1\tpouet:0,0,0,19,\tdownload:http://x/d.zip\n"),
    )]
    .into();
    let mut navigator = Navigator::new();
    setup_navigator(&dbs, &mut navigator).unwrap();

    navigator.goto("Demozoo/Awards");
    assert_eq!(
        navigator.get_showing(),
        vec!["Scene.org 2008", "The Meteoriks 2025"]
    );
    navigator.goto("Demozoo/Awards/Scene.org 2008");
    assert_eq!(
        navigator.get_showing(),
        vec!["Best direction", "Most original concept"]
    );
    navigator.goto("Demozoo/Awards/The Meteoriks 2025/Best High-end demo");
    assert_eq!(navigator.get_showing(), vec!["C / I", "D / J", "B / H"]);

    let ids = |pattern| -> Vec<&str> {
        let files = navigator.files_under(pattern);
        files.iter().map(|f| f.get_meta("id")).collect()
    };
    assert_eq!(ids("Demozoo/Awards/Scene.org *"), vec!["1", "1"]);
    assert_eq!(ids("Demozoo/Awards/*2025"), vec!["3", "4", "2"]);
    assert_eq!(
        ids("Demozoo/Awards/Scene.org 2008/Best direction"),
        vec!["1"]
    );
    assert!(ids("Demozoo/Awards/Nope *").is_empty());
}

#[test]
fn compo_with_slash_in_name() {
    let dbs: IndexMap<String, &'static [EmuFile]> = [(
        "Demozoo".to_string(),
        db("id:1\ttitle:A\tauthor:G\tparty:Evoke 2005;Oldskool 4k/Intro;1\tdownload:http://x/a.zip\n"),
    )]
    .into();
    let mut navigator = Navigator::new();
    setup_navigator(&dbs, &mut navigator).unwrap();

    navigator.goto("Demozoo/Parties/Evoke 2005");
    assert_eq!(navigator.get_showing(), vec!["Oldskool 4k/Intro"]);
    navigator.enter(&["Oldskool 4k/Intro"]);
    assert_eq!(navigator.current_path().len(), 4);
    assert_eq!(navigator.get_showing(), vec!["A / G"]);
    let files = navigator.files_under("Demozoo/Parties/Evoke 2005/*");
    assert_eq!(files.len(), 1);
}

#[test]
fn glob() {
    use super::glob_match;
    assert!(glob_match("Scene.org *", "Scene.org 2010"));
    assert!(!glob_match("Scene.org *", "The Meteoriks 2025"));
    assert!(glob_match("*20*0", "Scene.org 2010"));
    assert!(glob_match("*", ""));
    assert!(!glob_match("a*b", "ab b c"));
    assert!(glob_match("exact", "exact"));
}
