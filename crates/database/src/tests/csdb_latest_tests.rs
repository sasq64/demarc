use super::*;

fn sample() -> Vec<EmuFile> {
    let path = concat!(env!("DEMARC_ROOT"), "/testdata/csdb_latest.html");
    parse(&std::fs::read_to_string(path).unwrap())
}

#[test]
fn keeps_watchable_rows_with_downloads() {
    let files = sample();
    let titles: Vec<_> = files.iter().map(|f| f.game_info.title).collect();
    assert_eq!(
        titles,
        [
            "Carl Finlow - Anomaly [2sid]",
            "Max & Intro - Ostavi sve [2sid]",
            "Markku Pieraisi Ryhmäkuvassa",
        ]
    );
}

#[test]
fn fills_in_fields() {
    let f = &sample()[0];
    assert_eq!(f.get_meta("id"), "265811");
    assert_eq!(f.get_meta("author"), "Hokuto Force & TempesT");
    assert_eq!(f.get_meta("date"), "2026-11-11");
    assert_eq!(f.game_info.year(), 2026);
    assert_eq!(*f.categories, ["Music"]);
    assert_eq!(*f.platforms, ["C64"]);
    let FileSource::Url(urls) = &f.path else {
        panic!("not a url: {:?}", f.path);
    };
    assert_eq!(
        urls.as_slice(),
        ["https://csdb.dk/release/download.php?id=327348"]
    );
}

#[test]
fn decodes_entities() {
    assert_eq!(
        decode_entities("a &amp; b &#33; &#x41; &ouml;&Aring; &bogus c&"),
        "a & b ! A öÅ &bogus c&"
    );
}
