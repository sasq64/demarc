use anyhow::{Result, anyhow};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use tracing::warn;
use url::Url;

use crate::fetch::{OnProgress, fetch_url_with_progress, fetch_urls};

/// How many downloads are in flight right now, across every emulator.
///
/// Global rather than per-[`Emulator`](crate::emulator::Emulator) because the
/// UI that shows it ([`crate::egui_ui`]) draws one indicator for the whole
/// window and has no emulator to ask; kept in step by
/// [`download_started`]/[`download_finished`] around the job in
/// [`Emulator::load_async`](crate::emulator::Emulator::load_async).
static DOWNLOADS_IN_PROGRESS: AtomicUsize = AtomicUsize::new(0);

/// Count one more download as started.
pub fn download_started() {
    DOWNLOADS_IN_PROGRESS.fetch_add(1, Ordering::Relaxed);
}

/// Count one download as finished, however it ended -- landed, failed or
/// cancelled. Saturates at zero so a stray extra call can't wrap the counter
/// around into a permanent "downloading" state.
pub fn download_finished() {
    let _ = DOWNLOADS_IN_PROGRESS.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
        Some(n.saturating_sub(1))
    });
}

/// Downloads currently in flight; zero when nothing is loading.
pub fn downloads_in_progress() -> usize {
    DOWNLOADS_IN_PROGRESS.load(Ordering::Relaxed)
}

/// The download URLs of one release, kept as the `&'static str` slices they
/// were parsed out of rather than as [`Url`]s.
///
/// A db is mostly URLs — one line often carries several — and a parsed [`Url`]
/// costs a `String` plus the byte offsets into it, where the text it was sliced
/// out of is already `'static` and alive for the whole run (see [`EmuFile`]).
/// So the URLs are parsed once on the way in, to warn about and drop anything
/// that isn't one, and the text of the survivors is what gets kept.
///
/// Nearly everything downstream wants that text and nothing more, fetching
/// included — though what actually reaches the download cache is normalized
/// first, see [`cache_keys`]. The one caller that needs to take a URL apart
/// parses on demand — see [`Self::urls`].
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct UrlList(Vec<&'static str>);

impl UrlList {
    /// The URLs of a db `download` field: `;`-separated, with anything that
    /// doesn't parse as a URL logged and left out. An empty list means the
    /// entry has nothing to fetch and is dropped by the caller.
    pub fn parse_field(field: &'static str) -> Self {
        Self(
            field
                .split(';')
                .filter(|p| match Url::parse(p) {
                    Ok(_) => true,
                    Err(err) => {
                        warn!("Skipping unparseable URL {p:?}: {err}");
                        false
                    }
                })
                .collect(),
        )
    }

    /// A list of the one URL that was picked out of a longer one, so a load
    /// fetches that and nothing else.
    pub fn one(url: &'static str) -> Self {
        Self(vec![url])
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn first(&self) -> Option<&'static str> {
        self.get(0)
    }

    pub fn get(&self, index: usize) -> Option<&'static str> {
        self.0.get(index).copied()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'static str> + '_ {
        self.0.iter().copied()
    }

    pub fn as_slice(&self) -> &[&'static str] {
        &self.0
    }

    /// The URLs parsed, for a caller that needs more than the text — taking a
    /// URL apart into its path segments, say. Every entry parsed once already
    /// (see [`Self::parse_field`]), so this cannot fail.
    pub fn urls(&self) -> Vec<Url> {
        self.iter().filter_map(|u| Url::parse(u).ok()).collect()
    }
}

impl From<Vec<&'static str>> for UrlList {
    fn from(urls: Vec<&'static str>) -> Self {
        Self(urls)
    }
}

/// Where an [`EmuFile`]'s data comes from: either an already-local path or one
/// or more remote URLs that are downloaded on demand (see [`FileSource::resolve`]).
#[derive(Clone, Debug)]
pub enum FileSource {
    Url(UrlList),
    Path(PathBuf),
}

impl Default for FileSource {
    fn default() -> Self {
        FileSource::Path(PathBuf::new())
    }
}

impl From<PathBuf> for FileSource {
    fn from(path: PathBuf) -> Self {
        FileSource::Path(path)
    }
}

impl From<&Path> for FileSource {
    fn from(path: &Path) -> Self {
        FileSource::Path(path.to_owned())
    }
}

/// The lowercase extension of a URL's *path*, so a `?query` or `#fragment`
/// trailing the file name can't be mistaken for one.
fn url_extension(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    Path::new(url.path())
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

fn is_disk_image_url(url: &str) -> bool {
    const DISK_IMAGE_EXTENSIONS: [&str; 10] = [
        "d64", "d81", "adf", "dms", "msa", "st", "atr", "xex", "cue", "chd",
    ];
    url_extension(url).is_some_and(|e| DISK_IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// Narrow the URLs of one release down to the ones worth downloading.
///
/// A release listing often mixes the actual program with extras (music rips,
/// scans, ...). If any URL is a disk image, the release is disk based and only
/// the disk images are kept, so a multi-disk set stays together. Otherwise only
/// the obviously non-loadable extras are dropped.
///
/// Disk images are kept whatever their format, since the disks of one set may
/// well be archived differently: Hardwired by The Silents & Crionics has side A
/// as a `.dms` and side B as an `.adf`, and keying on the extension alone would
/// silently fetch only one of the two.
///
/// Filtering everything away would leave nothing to fetch, so a filter that
/// empties the list is dropped and the URLs are returned as they came in.
pub fn filter_release_urls<'a>(urls: &[&'a str]) -> Vec<&'a str> {
    /// Extensions that are never the main file of a release.
    const IGNORED_EXTENSIONS: [&str; 3] = ["sid", "pdf", "rtf"];

    let mut images: Vec<&'a str> = urls
        .iter()
        .copied()
        .filter(|u| is_disk_image_url(u))
        .collect();

    if images.is_empty() {
        images = urls
            .iter()
            .copied()
            .filter(|u| !url_extension(u).is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.as_str())))
            .collect();
    };
    if images.is_empty() { urls.to_vec() } else { images }
}

/// Download the first of `urls` that succeeds, using `fetch` for each attempt.
///
/// The URLs a release survives [`filter_release_urls`] with are alternatives —
/// a mirror, a reupload, the same file under a second link class — so a link
/// that is dead, 404s or times out only rules out that one URL, not the whole
/// release. Every failure is logged and the last one is returned if none of
/// them work; with nothing downloaded the caller has only one failure to
/// report, and the last is the one that ran out of alternatives.
///
/// `fetch` is a parameter so the walk can be tested without a network.
fn fetch_first_available(
    urls: &[impl AsRef<str>],
    on_progress: OnProgress<'_>,
    mut fetch: impl FnMut(&str, OnProgress<'_>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let mut last_error = None;
    for url in urls {
        let url = url.as_ref();
        match fetch(url, on_progress) {
            Ok(path) => return Ok(path),
            Err(e) => {
                warn!("download failed for {url}: {e:#}");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no download url")))
}

/// The URLs in the form the download cache keys its entries on.
///
/// NOTE: that form is the *normalized* URL — what [`Url`] prints back — not the
/// db's own text, which is what [`UrlList`] holds since it stopped keeping
/// parsed [`Url`]s. The two differ wherever parsing rewrites a URL (a bare host
/// gaining its trailing slash, an escape being canonicalised), and feeding the
/// raw text to [`crate::fetch`] would orphan every entry downloaded up to now
/// and quietly re-fetch it. Normalizing here keeps the existing cache matching.
///
/// The comment on [`crate::fetch::fetch_url_with_progress`] describes the key
/// as the URL as the db writes it, which is what the raw text would give — so
/// this is the thing to drop if the cache is ever allowed to churn once.
///
/// A URL that doesn't parse is passed through untouched. [`UrlList`] drops
/// those on the way in, so this only stands in for the lists built by hand.
fn cache_keys(urls: &[&str]) -> Vec<String> {
    urls.iter()
        .map(|u| Url::parse(u).map_or_else(|_| (*u).to_owned(), |parsed| parsed.to_string()))
        .collect()
}

impl FileSource {
    /// Ensure the data is available locally — downloading the URL (cached, see
    /// [`crate::fetch::fetch_url`]) the first time — and return the resulting
    /// local path. A [`FileSource::Path`] is returned as-is.
    ///
    /// This blocks for as long as the download takes, so on the main thread it
    /// is only safe for a source that is already a path. Loads that may hit the
    /// network go through [`Emulator::load_async`](crate::emulator::Emulator::load_async),
    /// which runs this on the I/O pool.
    pub fn resolve(&mut self) -> Result<&PathBuf> {
        self.resolve_with_progress(&|_, _| {})
    }

    /// [`resolve`](Self::resolve) reporting download progress, for the
    /// background job that a URL-backed load runs on.
    ///
    /// A multi-disk set reports nothing: it is several downloads in a row, and
    /// forwarding each one's byte count would restart the bar on every disk.
    /// A single file falling back to the next URL after a failure does restart
    /// it, which is the honest thing to show — that transfer really is starting
    /// over somewhere else.
    pub fn resolve_with_progress(&mut self, on_progress: OnProgress<'_>) -> Result<&PathBuf> {
        if let FileSource::Url(urls) = self {
            // If any URL is a disk image, this is a (possibly multi-) disk set:
            // download every disk image so they sit together in one directory
            // (built into an m3u later). Otherwise take the first entry that
            // downloads, treating the rest as fallbacks.
            let picked = filter_release_urls(urls.as_slice());
            // Normalized rather than passed on as they are stored — see
            // `cache_keys` for why the cache wants that form.
            let urls = cache_keys(&picked);
            let p = if urls.iter().any(|u| is_disk_image_url(u)) {
                fetch_urls(&urls)?
            } else {
                fetch_first_available(&urls, on_progress, fetch_url_with_progress)?
            };
            *self = FileSource::Path(p);
        }
        match self {
            FileSource::Path(p) => Ok(p),
            FileSource::Url(_) => unreachable!("just converted to Path above"),
        }
    }
}

/// The whole file list lives for the run (see [`EmuFile`]), so the strings here
/// are `&'static str` — either literals, slices of the leaked db text, or
/// individually leaked (see [`crate::files`]).
#[derive(Default, Debug, Clone, Copy)]
pub struct GameInfo {
    pub title: &'static str,
    pub group: &'static str,
    pub year: u32,
    pub category: &'static str,
}

// EmuFile can be:
// * Single PRG, ADF or other
// * Parsed M3U for loading
//   - Amiga or C64 with disks listed, path = m3u
// * Parsed M3U but not supported for loading
//   - No files listed, path = directory
// * Directory (if leaf)
// * Archive (sytem type unknown)

/// The list of these is built once at startup and kept for the whole run, so
/// every string an entry holds is `&'static str`: the db is read into a leaked
/// text the fields are sliced out of, and the few strings built at runtime
/// (m3u tags, file stems) are leaked one by one. That keeps entries cheap to
/// clone and hand around — no per-entry `String` allocations at all.
#[derive(Default, Clone, Debug)]
pub struct EmuFile {
    pub path: FileSource,
    pub meta: HashMap<&'static str, &'static str>,
    pub game_info: GameInfo,
}

impl EmuFile {
    pub fn get_meta(&self, name: &str) -> &'static str {
        self.meta.get(name).copied().unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Run `fetch_first_available` over `urls`, failing every URL whose path
    /// isn't `works`, and report which URLs were tried alongside the result.
    fn walk(urls: &[&str], works: &str) -> (Vec<String>, Result<PathBuf>) {
        let tried = RefCell::new(Vec::new());
        let result = fetch_first_available(urls, &|_, _| {}, |url, _| {
            tried.borrow_mut().push(url.to_string());
            if url.ends_with(works) {
                Ok(PathBuf::from(works))
            } else {
                Err(anyhow!("{url} is dead"))
            }
        });
        (tried.into_inner(), result)
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

    /// Nothing to try is an error rather than a panic — an entry whose URLs all
    /// got filtered away shouldn't take the process down.
    #[test]
    fn no_urls_is_an_error() {
        let (tried, result) = walk(&[], "demo.zip");
        assert!(tried.is_empty());
        assert!(result.is_err());
    }
}
