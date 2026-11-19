use super::*;
use database::files::{DbFilter, collect_db_text};

fn db() -> Vec<EmuFile> {
    let mut files = vec![];
    collect_db_text(
        "id:1\ttitle:A\tauthor:Booze Design & Censor Design\tparty:X'2008;C64 Demo;1\tdownload:http://x/a.zip\n",
        &DbFilter::default(),
        &mut files,
    );
    files
}

#[test]
fn event_opens_the_party() {
    assert_eq!(
        path_in("event", "X'2008", &db()),
        Some(vec!["Parties".into(), "X'2008".into()])
    );
}

#[test]
fn group_opens_its_releases() {
    assert_eq!(
        path_in("group", "Censor Design", &db()),
        Some(vec!["Authors".into(), "Censor Design".into()])
    );
}

#[test]
fn pages_without_releases_have_no_path() {
    assert_eq!(path_in("event", "Revision 2020", &db()), None);
    assert_eq!(path_in("group", "Oxyron", &db()), None);
    assert_eq!(path_in("release", "A", &db()), None);
}
