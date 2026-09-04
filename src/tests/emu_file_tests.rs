use super::*;
use std::cell::RefCell;

/// Run `fetch_first_available` over `urls`, failing every URL whose path
/// isn't `works`, and report which URLs were tried alongside the result.
fn walk(urls: &[&str], works: &str) -> (Vec<String>, Result<PathBuf>) {
    let tried = RefCell::new(Vec::new());
    let result = fetch_first_available(urls, &|_, _| {}, &mut |url: &str, _| {
        tried.borrow_mut().push(url.to_string());
        if url.ends_with(works) {
            Ok(PathBuf::from(works))
        } else {
            Err(anyhow!("{url} is dead"))
        }
    });
    (tried.into_inner(), result)
}

/// Run `fetch_release` over the downloads of a release, with `dead` naming
/// the URLs that fail; a URL that works downloads a file named after its
/// last path segment. Reports the URLs tried alongside the result.
fn load(urls: &[&str], dead: &[&str]) -> (Vec<String>, Result<PathBuf>) {
    let cache = tempfile::tempdir().expect("temp dir");
    let tried = RefCell::new(Vec::new());
    let result = fetch_release(&release_downloads(urls), &|_, _| {}, |url: &str, _| {
        tried.borrow_mut().push(url.to_string());
        if dead.iter().any(|d| url.ends_with(d)) {
            return Err(anyhow!("{url} is dead"));
        }
        let path = cache.path().join(url_file_name(url).expect("a file name"));
        std::fs::write(&path, url.as_bytes())?;
        Ok(path)
    });
    (tried.into_inner(), result)
}

/// The names of the files in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("a directory")
        .map(|e| {
            e.expect("an entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

/// A dead first link only rules out that URL: the next one is tried, and
/// nothing past the one that works is touched.
#[test]
fn a_failed_download_falls_back_to_the_next_url() {
    let (tried, result) = walk(
        &[
            "https://dead.example/demo.zip",
            "https://mirror.example/demo.lha",
            "https://never.example/demo.lzx",
        ],
        "demo.lha",
    );
    assert_eq!(result.unwrap(), PathBuf::from("demo.lha"));
    assert_eq!(
        tried,
        vec![
            "https://dead.example/demo.zip",
            "https://mirror.example/demo.lha"
        ]
    );
}

/// With every URL dead the walk reports the last failure, so the message
/// the user sees comes from the attempt that ran out of alternatives.
#[test]
fn all_urls_failing_reports_the_last_failure() {
    let (tried, result) = walk(
        &["https://a.example/demo.zip", "https://b.example/demo.zip"],
        "nothing.zip",
    );
    assert_eq!(tried.len(), 2);
    assert_eq!(
        result.unwrap_err().to_string(),
        "https://b.example/demo.zip is dead"
    );
}

/// A disk set that can't be had at all falls through to the release's
/// other downloads: D.O.S. by Andromeda is listed as one disk in two
/// formats plus a `.zip`, and with amigascne down the `.zip` still loads.
#[test]
fn a_dead_disk_set_falls_back_to_the_next_download() {
    let (tried, result) = load(
        &[
            "AmigascneFile:/Groups/A/Andromeda/Andromeda-dos.adf",
            "AmigascneFile:/Groups/A/Andromeda/ANDROMEDA-DOS.dms",
            "SceneOrgFile:/parties/1992/thegathering92/amiga_demo/andromeda-d_o_s.zip",
        ],
        &["Andromeda-dos.adf", "ANDROMEDA-DOS.dms"],
    );
    assert_eq!(tried.len(), 3, "every download tried: {tried:?}");
    assert_eq!(result.unwrap().file_name().unwrap(), "andromeda-d_o_s.zip");
}

/// One disk in two formats is one disk: the second format is tried when the
/// first is dead, and the set is complete with just the one that worked.
#[test]
fn a_dead_disk_falls_back_to_its_other_format() {
    let (tried, result) = load(
        &[
            "https://a.example/Andromeda-dos.adf",
            "https://b.example/ANDROMEDA-DOS.dms",
        ],
        &["Andromeda-dos.adf"],
    );
    assert_eq!(tried.len(), 2);
    assert_eq!(names(&result.unwrap()), vec!["ANDROMEDA-DOS.dms"]);
}

/// Differently named images are the disks of one set, so all of them are
/// fetched — into one directory, whatever format each disk is in.
#[test]
fn a_disk_set_fetches_every_disk() {
    let (_, result) = load(
        &[
            "https://a.example/hardwired-1.dms",
            "https://a.example/hardwired-2.adf",
        ],
        &[],
    );
    assert_eq!(
        names(&result.unwrap()),
        vec!["hardwired-1.dms", "hardwired-2.adf"]
    );
}

/// A disk with no working copy anywhere fails the whole set rather than
/// booting half of it — and then the release's other downloads get a go.
#[test]
fn a_missing_disk_fails_the_whole_set() {
    let (_, result) = load(
        &[
            "https://a.example/hardwired-1.dms",
            "https://a.example/hardwired-2.adf",
            "https://a.example/hardwired.zip",
        ],
        &["hardwired-2.adf"],
    );
    assert_eq!(result.unwrap().file_name().unwrap(), "hardwired.zip");
}

/// An override naming one of a release's downloads narrows the list to it,
/// so the load fetches the demo rather than the soundtrack beside it. The
/// name is matched against the URL's own file name, whatever the scheme —
/// a demozoo db writes half its links as `SceneOrgFile:/…`.
#[test]
fn an_override_picks_the_download_it_names() {
    let listing = || {
        FileSource::Url(
            vec![
                "https://media.demozoo.org/music/tune.mp3",
                "SceneOrgFile:/parties/2000/mekka/inside.zip",
            ]
            .into(),
        )
    };

    let mut source = listing();
    // Written in whatever case the release's own docs use.
    source.pick_download("INSIDE.ZIP");
    let FileSource::Url(urls) = &source else {
        panic!("still a URL list, {source:?}")
    };
    assert_eq!(
        urls.as_slice(),
        ["SceneOrgFile:/parties/2000/mekka/inside.zip"]
    );

    // A name that matches nothing — a mirror renamed the file since the
    // override was written — leaves the list to be guessed at as before.
    let mut source = listing();
    source.pick_download("gone.zip");
    let FileSource::Url(urls) = &source else {
        panic!("still a URL list, {source:?}")
    };
    assert_eq!(urls.len(), 2);
}

/// Nothing to try is an error rather than a panic — an entry whose URLs all
/// got filtered away shouldn't take the process down.
#[test]
fn no_urls_is_an_error() {
    let (tried, result) = walk(&[], "demo.zip");
    assert!(tried.is_empty());
    assert!(result.is_err());
}
