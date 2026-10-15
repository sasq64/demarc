use super::*;

fn song(path: &str) -> (String, String, &str) {
    let song = parse_path(path, &HashSet::new());
    (song.title, song.author, song.platform)
}

#[test]
fn a_song_is_named_after_its_path() {
    assert_eq!(
        song("Protracker/Tip/coop-Kid Frost/sand in her hair.mod"),
        (
            "Sand in her hair".into(),
            "Tip & Kid Frost".into(),
            "Protracker"
        )
    );
    assert_eq!(
        song("NoiseTrekker/- unknown/robot-ballet.ntk"),
        ("Robot-ballet".into(), "Unknown".into(), "NoiseTrekker")
    );
    assert_eq!(
        song("Nintendo SPC/Andy Brock/Judge Dredd/02 - level intro 1.spc"),
        (
            "Judge Dredd (02 - level intro 1)".into(),
            "Andy Brock".into(),
            "Nintendo SPC"
        )
    );
    assert_eq!(
        song("Nintendo SPC/Tim Follin/coop-Geoff Follin/Spider-Man/06 - storm.spc"),
        (
            "Spider-Man (06 - storm)".into(),
            "Tim Follin & Geoff Follin".into(),
            "Nintendo SPC"
        )
    );
    assert_eq!(
        song("Spectrum/Pro Tracker 3/Factor6/amigaintro.pt3"),
        ("Amigaintro".into(), "Factor6".into(), "Pro Tracker 3")
    );
    assert_eq!(
        song("His Master's Noise/Mahoney/coop Flex/waffle.mod"),
        (
            "Waffle".into(),
            "Mahoney & Flex".into(),
            "His Master's Noise"
        )
    );
    assert_eq!(
        song("Video Game Music/Sega Megadrive/- unknown/Battle Golfer Yui/21 due.vgz"),
        (
            "Battle Golfer Yui (21 due)".into(),
            "Unknown".into(),
            "Sega Megadrive"
        )
    );
    assert_eq!(
        song("Playstation Sound Format/Nobuo Uematsu/Final Fantasy 7/Disc 1/101.psf"),
        (
            "Final Fantasy 7 (Disc 1 / 101)".into(),
            "Nobuo Uematsu".into(),
            "Playstation Sound Format"
        )
    );
    assert_eq!(
        song("Nintendo SPC/Tim Follin/plok.spc"),
        ("Plok".into(), "Tim Follin".into(), "Nintendo SPC")
    );
    assert_eq!(
        song("Ashley Hogg/ash.cj in the usa"),
        ("Ash".into(), "Ashley Hogg".into(), "")
    );
}

#[test]
fn collections_have_their_own_layout() {
    assert_eq!(
        song("HVSC/MUSICIANS/D/DRAX/Worktunes/Conversion_from_Soundmonitor.sid"),
        ("Conversion from Soundmonitor".into(), "DRAX".into(), "HVSC")
    );
    assert_eq!(
        song("HVSC/DEMOS/S-Z/Xmas-Card_BASIC.sid"),
        ("Xmas-Card BASIC".into(), "".into(), "HVSC")
    );
    assert_eq!(
        song("SNDH/Mad_Max/Demos/SID/Airball_Remix.sndh"),
        ("Airball Remix".into(), "Mad Max".into(), "SNDH")
    );
    assert_eq!(
        song("SNDH/4-Mat/Beastbusters.sndh"),
        ("Beastbusters".into(), "4-Mat".into(), "SNDH")
    );
}

#[test]
fn an_index_is_told_from_a_db() {
    assert!(is_index("\n259547\tAce Tracker/505/blocks.am\n"));
    assert!(!is_index(
        "# Demozoo\nid:1\ttitle:A\tdownload:http://a/b.zip\n"
    ));
    assert!(!is_index("id:1\ttitle:A\n"));
}

#[test]
fn collect_builds_encoded_urls() {
    let mut out = vec![];
    collect(
        "14779\tAHX/MJK/noisy pillars 99%.ahx\n\
         66418\tProtracker/Dalezy/cheating engine tune #1.MOD\n\
         100\tAd Lib/AMusic/Dalezy/unsupported.amd\n",
        &DbFilter::default(),
        &mut out,
    );
    assert_eq!(out.len(), 2);
    let FileSource::Url(urls) = &out[0].path else {
        panic!("not a URL: {:?}", out[0].path);
    };
    assert_eq!(
        urls.first(),
        Some("https://modland.com/pub/modules/AHX/MJK/noisy%20pillars%2099%25.ahx")
    );
    assert_eq!(out[0].game_info.title, "Noisy pillars 99%");
    assert_eq!(out[0].game_info.group, "MJK");
    assert_eq!(out[0].get_meta("platform"), "AHX");
    assert!(out[1].get_meta("download").ends_with("tune%20%231.MOD"));
}

#[test]
fn an_hvsc_name_is_turned_to_match_another_directory() {
    let mut out = vec![];
    collect(
        "1\tHVSC/MUSICIANS/T/Tel_Jeroen/Cybernoid.sid\n\
         1\tHVSC/MUSICIANS/M/Mass_Murderer/Tune.sid\n\
         1\tHVSC/MUSICIANS/M/Mad_Max/Tune.sid\n\
         1\tHVSC/MUSICIANS/0-9/20CC/van_Santen_Edwin/Tune.sid\n\
         1\tProtracker/Jeroen Tel/tune.mod\n\
         1\tAHX/Max Mad/coop-Mad Max/tune.ahx\n",
        &DbFilter::default(),
        &mut out,
    );
    let authors: Vec<_> = out.iter().map(|f| f.game_info.group).collect();
    assert_eq!(
        authors,
        [
            "Jeroen Tel",
            "Mass Murderer",
            "Mad Max",
            "20CC",
            "Jeroen Tel",
            "Max Mad & Mad Max"
        ]
    );
}
