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

/// Where an [`EmuFile`]'s data comes from: either an already-local path or one
/// or more remote URLs that are downloaded on demand (see [`FileSource::resolve`]).
#[derive(Clone, Debug)]
pub enum FileSource {
    Url(Vec<Url>),
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

fn url_extension(url: &Url) -> Option<String> {
    Path::new(url.path())
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

fn is_disk_image(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        return [
            "d64", "d81", "adf", "dms", "msa", "st", "atr", "xex", "cue", "chd",
        ]
        .contains(&ext.as_str());
    }
    false
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
pub fn filter_release_urls(urls: Vec<Url>) -> Vec<Url> {
    /// Extensions that are never the main file of a release.
    const IGNORED_EXTENSIONS: [&str; 3] = ["sid", "pdf", "rtf"];

    let mut images: Vec<Url> = urls
        .iter()
        .filter(|u| is_disk_image(Path::new(u.path())))
        .cloned()
        .collect();

    if images.is_empty() {
        images = urls
            .iter()
            .filter(|u| !url_extension(u).is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.as_str())))
            .cloned()
            .collect();
    };
    if images.is_empty() { urls } else { images }
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
    urls: &[Url],
    on_progress: OnProgress<'_>,
    mut fetch: impl FnMut(&str, OnProgress<'_>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let mut last_error = None;
    for url in urls {
        match fetch(url.as_ref(), on_progress) {
            Ok(path) => return Ok(path),
            Err(e) => {
                warn!("download failed for {url}: {e:#}");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("no download url")))
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
            let urls = filter_release_urls(urls.clone());
            let p = if urls.iter().any(|u| is_disk_image(Path::new(u.path()))) {
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

#[derive(Default, Debug, Clone)]
pub struct GameInfo {
    pub title: String,
    pub group: String,
    pub year: u32,
    pub category: String,
}

// EmuFile can be:
// * Single PRG, ADF or other
// * Parsed M3U for loading
//   - Amiga or C64 with disks listed, path = m3u
// * Parsed M3U but not supported for loading
//   - No files listed, path = directory
// * Directory (if leaf)
// * Archive (sytem type unknown)

#[derive(Default, Clone, Debug)]
pub struct EmuFile {
    pub path: FileSource,
    pub meta: HashMap<String, String>,
    pub game_info: GameInfo,
}

impl EmuFile {
    pub fn get_meta(&self, name: impl Into<String>) -> String {
        self.meta
            .get(&name.into())
            .map_or("".into(), |s| s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Run `fetch_first_available` over `urls`, failing every URL whose path
    /// isn't `works`, and report which URLs were tried alongside the result.
    fn walk(urls: &[&str], works: &str) -> (Vec<String>, Result<PathBuf>) {
        let urls: Vec<Url> = urls.iter().map(|u| Url::parse(u).unwrap()).collect();
        let tried = RefCell::new(Vec::new());
        let result = fetch_first_available(&urls, &|_, _| {}, |url, _| {
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
