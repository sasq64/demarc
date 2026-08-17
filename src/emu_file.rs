use anyhow::Result;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use url::Url;

use crate::fetch::{OnProgress, fetch_url_with_progress, fetch_urls};

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
    const IGNORED_EXTENSIONS: [&str; 2] = ["sid", "pdf"];

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
    pub fn resolve_with_progress(&mut self, on_progress: OnProgress<'_>) -> Result<&PathBuf> {
        if let FileSource::Url(urls) = self {
            // If any URL is a disk image, this is a (possibly multi-) disk set:
            // download every disk image so they sit together in one directory
            // (built into an m3u later). Otherwise just grab the first entry.
            let urls = filter_release_urls(urls.clone());
            let p = if urls.iter().any(|u| is_disk_image(Path::new(u.path()))) {
                fetch_urls(&urls)?
            } else {
                fetch_url_with_progress(urls.first().unwrap().as_ref(), on_progress)?
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
