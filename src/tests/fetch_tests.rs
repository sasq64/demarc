use super::*;

/// Download `url` the way the rest of demarc does — through the mirror
/// walk, into a file — and hand back the bytes that landed there.
fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("out.bin");
    download_to(url, &path, &|_, _| {})?;
    Ok(std::fs::read(path)?)
}

/// The `base`th mirror of `url`'s class, counted in [`LINK_BASES`] order
/// rather than in whatever order [`MIRROR_ROTATION`] currently prefers.
fn mirror(url: &str, base: usize) -> Mirror {
    let (class, _) = link_class(url).unwrap();
    Mirror { class, base }
}

#[test]
fn detects_urls() {
    assert!(is_url("https://example.com/a.zip"));
    assert!(is_url("http://example.com/a.zip"));
    assert!(is_url("ftp://example.com/a.zip"));
    assert!(is_url("SceneOrgFile:/parties/2006/x.zip"));
    assert!(!is_url("/home/user/a.zip"));
    assert!(!is_url("a.zip"));
    assert!(!is_url("C:/games/a.zip"));
}

#[test]
fn resolves_a_link_class_to_its_mirrors() {
    let urls = resolve_url("SceneOrgFile:/parties/2006/assembly06/demo/x.zip");
    assert_eq!(
        urls,
        vec![
            "https://files.scene.org/get:de-https/parties/2006/assembly06/demo/x.zip",
            "https://files.scene.org/get:fi-ftp/parties/2006/assembly06/demo/x.zip",
        ]
    );
    // A base that is not a directory prefix joins just as directly.
    assert_eq!(
        resolve_url("Defacto2File:a53998"),
        vec!["https://defacto2.net/f/a53998"]
    );
}

/// A db line reaches us through `Url::parse`, which lowercases the scheme,
/// so the class has to match however it was spelled.
#[test]
fn resolves_a_link_class_case_insensitively() {
    let parsed = Url::parse("ModlandFile:/pub/modules/Protracker/Wal/raw.mod").unwrap();
    assert_eq!(parsed.scheme(), "modlandfile");
    assert_eq!(
        resolve_url(parsed.as_str()),
        vec!["https://ftp.modland.com/pub/modules/Protracker/Wal/raw.mod"]
    );
}

#[test]
fn rewrites_urls_that_no_longer_work_as_recorded() {
    // A plain url from a db, pointed at the mirror that serves it.
    assert_eq!(
        resolve_url("https://files.scene.org/get/demos/x.zip"),
        vec!["https://files.scene.org/get:de-https/demos/x.zip"]
    );
    // ...and the same fixup applied after a link class was resolved: these
    // parameters carry the base's own path prefix a second time.
    assert_eq!(
        resolve_url("ModlandFile:/pub/modules/pub/modules/Wal/raw.mod"),
        vec!["https://ftp.modland.com/pub/modules/Wal/raw.mod"]
    );
    // A url no rule matches is left exactly as it was.
    assert_eq!(
        resolve_url("https://example.com/a.zip"),
        vec!["https://example.com/a.zip"]
    );
}

/// A mirror that worked becomes the one the next download starts at. The
/// list keeps its cyclic order — this is a rotation, not a move to front —
/// so the mirrors after the winner stay in their table order.
///
/// Uses AmigascneFile, the one class with three mirrors, and puts the
/// rotation back afterwards: [`MIRROR_ROTATION`] is process-wide state.
#[test]
fn rotates_to_the_mirror_that_last_worked() {
    let url = "AmigascneFile:/Gfx/M/Mr_Acid/Count%20Duckula.png";
    let table = resolve_url(url);
    assert_eq!(table.len(), 3);

    promote_mirror(mirror(url, 2));
    assert_eq!(
        resolve_url(url),
        vec![table[2].clone(), table[0].clone(), table[1].clone()]
    );

    promote_mirror(mirror(url, 0));
    assert_eq!(resolve_url(url), table);
}

/// A url that is spelled out has no mirror to promote, so a download of one
/// leaves every class's rotation alone.
#[test]
fn a_plain_url_has_no_mirror() {
    let candidates = candidates("https://example.com/a.zip");
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].mirror.is_none());
}

/// The cache keys on the db's own url so a mirror change keeps the entry,
/// but the file inside it is named from the resolved url — dispatch keys on
/// the extension, and a link class has none.
#[test]
fn names_a_link_class_download_after_the_resolved_url() {
    assert_eq!(
        url_filename(&primary_url(
            "AmigascneFile:/Groups/D/DOC/DOC-Digidemo1.dms"
        )),
        "DOC-Digidemo1.dms"
    );
}

#[test]
#[ignore = "hits the network"]
fn downloads_https_to_ftp_redirect() {
    // An FTP mirror named directly: a bare `/get/` link redirects here too,
    // but [`URL_REWRITES`] sends that one to the HTTPS mirror instead.
    let buf = download(
        "https://files.scene.org/get:fi-ftp/demos/groups/dual_crew_shining/gbc/dcs-nmod.zip",
    )
    .unwrap();
    assert_eq!(buf.len(), 46596);
    assert_eq!(&buf[..2], b"PK");
}

/// A path with a space survives the https→ftp redirect: files.scene.org
/// sends the space raw in `Location`, `Url::join` encodes it to `%20`, and
/// the FTP side has to decode it again before `RETR` or the server 550s.
#[test]
#[ignore = "hits the network"]
fn downloads_ftp_path_containing_a_space() {
    let buf = download(
        "https://files.scene.org/get:fi-ftp/mirrors/amigascne/Gfx/M/Mr_Acid/Count%20Duckula.png",
    )
    .unwrap();
    assert_eq!(buf.len(), 5402);
    assert_eq!(&buf[1..4], b"PNG");
}

/// The whole path a db line takes: a link class is resolved to its mirror
/// and downloaded from there.
#[test]
#[ignore = "hits the network"]
fn downloads_a_link_class_url() {
    let buf =
        download("SceneOrgFile:/demos/groups/dual_crew_shining/gbc/dcs-nmod.zip").unwrap();
    assert_eq!(buf.len(), 46596);
    assert_eq!(&buf[..2], b"PK");
}

#[test]
#[ignore = "hits the network"]
fn caches_under_url_hash() {
    let url = "https://files.scene.org/get/demos/groups/dual_crew_shining/gbc/dcs-nmod.zip";
    let path = fetch_url(url).unwrap();
    assert_eq!(path.file_name().unwrap(), "dcs-nmod.zip");
    // The file keeps its readable name, but the directory holding it is
    // named for the url's hash, not for the file.
    let entry = path
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(entry.len(), 16);
    assert!(entry.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 46596);
    // Second call is a cache hit on the same path, no re-download.
    assert_eq!(fetch_url(url).unwrap(), path);
}

/// The byte count reported to `on_progress` accumulates across writes and
/// carries the declared total, which is what drives a download's progress
/// bar.
#[test]
fn counts_bytes_written() {
    let seen = std::sync::Mutex::new(Vec::new());
    let mut sink = Vec::new();
    let report = |done, total| seen.lock().unwrap().push((done, total));
    {
        let mut writer = CountingWriter::new(&mut sink, Some(4), &report);
        // Copy in two chunks so the running total has to be additive.
        std::io::copy(&mut &b"ab"[..], &mut writer).unwrap();
        std::io::copy(&mut &b"cd"[..], &mut writer).unwrap();
    }
    assert_eq!(sink, b"abcd");
    assert_eq!(seen.into_inner().unwrap(), vec![(2, Some(4)), (4, Some(4))]);
}

#[test]
fn extracts_filename() {
    assert_eq!(url_filename("https://x.com/path/foo.zip"), "foo.zip");
    assert_eq!(url_filename("https://x.com/path/foo.zip?a=b"), "foo.zip");
    // An already-encoded segment is decoded before re-encoding, so it comes
    // back as it went in rather than doubly encoded.
    assert_eq!(url_filename("https://x.com/foo%20bar.d64"), "foo%20bar.d64");
    assert_eq!(url_filename("https://x.com/a b&c.zip"), "a%20b%26c.zip");
    assert_eq!(url_filename("https://x.com/"), "download");
    assert_eq!(url_filename("game.zip"), "game.zip");
}
