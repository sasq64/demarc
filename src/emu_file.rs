use anyhow::Result;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use url::Url;

use crate::{
    fetch::{fetch_url, fetch_urls},
    systems::GameInfo,
};

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
    /// [`fetch_url`]) the first time — and return the resulting local path. A
    /// [`FileSource::Path`] is returned as-is.
    pub fn resolve(&mut self) -> Result<&PathBuf> {
        if let FileSource::Url(urls) = self {
            // If any URL is a disk image, this is a (possibly multi-) disk set:
            // download every disk image so they sit together in one directory
            // (built into an m3u later). Otherwise just grab the first entry.
            let urls = filter_release_urls(urls.clone());
            let p = if urls.iter().any(|u| is_disk_image(Path::new(u.path()))) {
                fetch_urls(&urls)?
            } else {
                fetch_url(urls.first().unwrap().as_ref())?
            };
            *self = FileSource::Path(p);
        }
        match self {
            FileSource::Path(p) => Ok(p),
            FileSource::Url(_) => unreachable!("just converted to Path above"),
        }
    }
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
    pub tags: HashMap<String, String>,
    pub game_info: GameInfo,
}

impl EmuFile {
    pub fn tag(&self, name: impl Into<String>) -> String {
        self.tags
            .get(&name.into())
            .map_or("".into(), |s| s.to_string())
    }
}
