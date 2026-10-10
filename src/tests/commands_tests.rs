use super::*;
use crate::emu_file::{GameInfo, UrlList};
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

/// The file picker is a list *of entries*: the id a row reports resolves
/// back to the entry itself, which is how the picker's caller gets at the
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
    let files: &'static [EmuFile] = Box::leak(Box::new([
        file("Zentrophy", URL),
        file("Deus Ex Machina", "https://a.org/d.lha"),
    ]));
    let source = PickerSource::new(files, None, IconMode::All);

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

#[test]
fn pad_modifier_picks_the_combined_mapping() {
    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::West);
    assert_eq!(check_pad(&pad), Some(Cmd::OpenFile));

    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::LeftTrigger2);
    assert_eq!(check_pad(&pad), None);
    pad.clear();
    pad.press(GamepadButton::West);
    assert_eq!(check_pad(&pad), Some(Cmd::ChangeScale));

    // A button with no combined mapping does nothing while the modifier is held.
    pad.clear();
    pad.press(GamepadButton::Start);
    assert_eq!(check_pad(&pad), None);
}

#[test]
fn pad_b_opens_the_keyboard() {
    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::East);
    assert_eq!(check_pad(&pad), Some(Cmd::OpenKeyboard));
}

#[test]
fn command_list_shows_pad_buttons() {
    let pad = command_list(true);
    assert_eq!(pad.len(), PAD_HOTKEYS.len());
    assert_eq!(pad[0], (" \u{f0c32}      Open file menu ".to_string(), Cmd::OpenFile));
    assert!(pad.iter().any(|(line, _)| line.starts_with(" L2+R1 ")));
    assert_eq!(command_list(false).len(), HOTKEYS.len());
}
