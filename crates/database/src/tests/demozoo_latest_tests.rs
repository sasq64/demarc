use super::*;

fn prod(id: u32) -> Value {
    let path = format!("{}/testdata/demozoo_latest/{id}.json", env!("DEMARC_ROOT"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn fills_in_fields() {
    let f = parse_production(&prod(400546)).unwrap();
    assert_eq!(f.get_meta("id"), "400546");
    assert_eq!(f.get_meta("title"), "Fire #47");
    assert_eq!(f.get_meta("author"), "Fire");
    assert_eq!(f.get_meta("date"), "2026-11-11");
    assert_eq!(f.get_meta("db"), "Demozoo");
    assert_eq!(*f.platforms, ["MS-Dos"]);
    assert_eq!(*f.categories, ["Artpack"]);
    let FileSource::Url(urls) = &f.path else {
        panic!("not a url: {:?}", f.path);
    };
    assert_eq!(urls.as_slice(), ["DemozooProduction:400546"]);
    assert_eq!(production_id(urls.as_slice()[0]), Some("400546"));
}

#[test]
fn scene_org_links_become_mirror_classes() {
    let mut p = prod(400824);
    p["download_links"] = serde_json::json!([
        {"link_class": "BaseUrl", "url": "https://example.com/a.mp4"},
        {"link_class": "BaseUrl", "url": "https://example.com/b.zip"},
        {"link_class": "SceneOrgFile", "url": "https://files.scene.org/view/parties/2026/x.zip"},
    ]);
    assert_eq!(
        downloads(&p).as_slice(),
        [
            "SceneOrgFile:/parties/2026/x.zip",
            "https://example.com/b.zip"
        ]
    );
    assert!(downloads(&prod(400824)).as_slice().is_empty());
}

#[test]
fn drops_unplayable_platforms_and_types() {
    let mut p = prod(400546);
    p["platforms"] = serde_json::json!([{"name": "Atari Jaguar"}]);
    assert!(parse_production(&p).is_none());
    let mut p = prod(400546);
    p["types"] = serde_json::json!([{"name": "Video"}]);
    assert!(parse_production(&p).is_none());
}

#[test]
fn key_round_trips() {
    assert!(api_key().bytes().all(|b| b.is_ascii_graphic()));
}
