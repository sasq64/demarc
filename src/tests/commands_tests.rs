use super::*;
use crate::emu_file::GameInfo;
use crate::fuzzy_list::DEFAULT_MAX_RESULTS;

const URL: &str = "https://ftp.example.org/pub/demos/c64/1992/zentro4.zip";

#[test]
fn short_url_is_left_alone() {
    assert_eq!(trunc_url(URL, URL.len()), URL);
    assert_eq!(trunc_url("http://a.org/x.zip", 70), "http://a.org/x.zip");
}

#[test]
fn path_components_drop_from_the_left_until_it_fits() {
    // One character short. Dropping `pub` alone buys nothing (`...` is just
    // as long), so `demos` goes with it — components come off the left
    // until the result actually fits.
    assert_eq!(
        trunc_url(URL, URL.len() - 1),
        "https://ftp.example.org/.../c64/1992/zentro4.zip"
    );
    // Tighter budgets eat further into the path, always from the left…
    assert_eq!(
        trunc_url(URL, 46),
        "https://ftp.example.org/.../1992/zentro4.zip"
    );
    // …down to just the host and the file name.
    assert_eq!(
        trunc_url(URL, 40),
        "https://ftp.example.org/.../zentro4.zip"
    );
}

#[test]
fn every_result_fits_the_budget() {
    for max in 4..URL.len() + 2 {
        let out = trunc_url(URL, max);
        assert!(
            out.chars().count() <= max,
            "{max}: {out:?} is {} chars",
            out.chars().count()
        );
    }
}

#[test]
fn host_and_file_too_long_together_are_cut_in_the_middle() {
    // Nothing left to drop, so both ends are kept and the middle goes.
    let out = trunc_url(URL, 20);
    assert_eq!(out.chars().count(), 20);
    assert!(out.starts_with("https://"), "{out}");
    assert!(out.ends_with(".zip"), "{out}");
}

#[test]
fn urls_without_a_path_are_still_bounded() {
    let out = trunc_url("https://a-very-long-host-name.example.org", 20);
    assert_eq!(out.chars().count(), 20);
}

#[test]
fn download_rows_are_the_file_name_part_of_the_url() {
    assert_eq!(url_file_name(&Url::parse(URL).unwrap()), "zentro4.zip");
    // Percent escapes are shown as the characters they stand for...
    assert_eq!(
        url_file_name(&Url::parse("https://a.org/d/Count%20Duckula.zip").unwrap()),
        "Count Duckula.zip"
    );
    // ...and a query string is not part of the name.
    assert_eq!(
        url_file_name(&Url::parse("https://a.org/get?id=1").unwrap()),
        "get"
    );
    // Nothing to name: the whole URL is listed instead.
    let dir = "https://a.org/pub/";
    assert_eq!(url_file_name(&Url::parse(dir).unwrap()), dir);
}

/// The download picker is only worth opening when the entry really has
/// alternatives to pick between.
#[test]
fn only_entries_with_several_urls_have_downloads_to_pick() {
    let file = |urls: Vec<&'static str>| EmuFile {
        path: FileSource::Url(urls.into()),
        ..Default::default()
    };
    assert!(DownloadSource::new(&EmuFile::default()).is_none());
    assert!(DownloadSource::new(&file(vec![URL])).is_none());

    let source =
        DownloadSource::new(&file(vec![URL, "https://mirror.example/demo.lha"])).unwrap();
    let rows = source.search("", DEFAULT_MAX_RESULTS);
    assert_eq!(
        rows.iter()
            .map(|&id| source.get_text(id))
            .collect::<Vec<_>>(),
        vec!["zentro4.zip", "demo.lha"]
    );
    // The id a row reports is its index into the entry's URLs, and the
    // info field spells the chosen one out in full.
    assert_eq!(source.get_info(rows[1]), "https://mirror.example/demo.lha");
}

/// The file picker is a list *of entries*: the id a row reports resolves
/// back to the entry itself, which is how [`original_file`] gets at the
/// snapshot rather than at whatever `settings.files` holds by now.
#[test]
fn the_file_picker_hands_the_entry_behind_a_row_back() {
    let file = |title: &'static str, url: &'static str| EmuFile {
        path: FileSource::Url(UrlList::one(url)),
        game_info: GameInfo {
            title,
            ..Default::default()
        },
        ..Default::default()
    };
    let files = [
        file("Zentrophy", URL),
        file("Deus Ex Machina", "https://a.org/d.lha"),
    ];
    let source = FilePickerSource::new(&files);

    let rows = source.search("machina", DEFAULT_MAX_RESULTS);
    assert_eq!(rows.len(), 1);
    assert_eq!(source.get_text(rows[0]), "Deus Ex Machina");
    // The row resolves to the entry, URLs and all.
    let entry = source.get_data(rows[0]).expect("the row is one of ours");
    assert_eq!(entry.game_info.title, "Deus Ex Machina");
    assert!(
        matches!(&entry.path, FileSource::Url(urls) if urls.first() == Some("https://a.org/d.lha"))
    );
    // An id that is not one of ours has nothing behind it.
    assert!(source.get_data(files.len()).is_none());
}

#[test]
fn multibyte_urls_are_counted_in_characters() {
    let url = "https://exämple.org/påth/före/filnämn-ÅÄÖ.zip";
    let out = trunc_url(url, 40);
    assert_eq!(out, "https://exämple.org/.../filnämn-ÅÄÖ.zip");
    assert!(out.chars().count() <= 40);
}
