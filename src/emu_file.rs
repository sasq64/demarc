use anyhow::{Context, Result, anyhow};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use tracing::warn;
use url::Url;

use crate::fetch::{OnProgress, fetch_url_with_progress};

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

/// The file name at the end of a URL's *path*, percent-decoded, so it can be
/// compared with the name an override names. `None` for a URL with no path at
/// all, e.g. `https://example.com`.
fn url_file_name(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let name = Path::new(url.path()).file_name()?.to_str()?;
    Some(
        percent_encoding::percent_decode_str(name)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

fn is_disk_image_url(url: &str) -> bool {
    const DISK_IMAGE_EXTENSIONS: [&str; 10] = [
        "d64", "d81", "adf", "dms", "msa", "st", "atr", "xex", "cue", "chd",
    ];
    url_extension(url).is_some_and(|e| DISK_IMAGE_EXTENSIONS.contains(&e.as_str()))
}

/// The file name of a disk image URL without its extension, lowercased — what
/// tells two copies of one disk apart from two disks of one set.
///
/// Demozoo lists both `Andromeda-dos.adf` and `ANDROMEDA-DOS.dms` for D.O.S. by
/// Andromeda: one disk, twice, in two archive formats. A real multi-disk set
/// numbers or names its disks apart instead, so their stems differ. A URL with
/// no file name at all keeps the whole URL, which groups with nothing.
fn disk_stem(url: &str) -> String {
    let name = url_file_name(url).unwrap_or_else(|| url.to_owned());
    let stem = Path::new(&name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&name);
    stem.to_ascii_lowercase()
}

/// One way of getting at a release's data, as [`release_downloads`] reads the
/// `download` field: a whole disk set, or a single file.
#[derive(Debug, PartialEq, Eq)]
pub enum Download<'a> {
    /// A disk set: one entry per disk, each holding that disk's alternatives
    /// best first. Every disk has to land — half a set won't boot — so the
    /// alternatives are what a dead link falls back on, and they are fetched
    /// side by side into one directory.
    Disks(Vec<Vec<&'a str>>),
    /// A single file: an archive, an executable, whatever the release ships as.
    File(&'a str),
}

impl Download<'_> {
    /// What to call this download in a log line.
    fn label(&self) -> String {
        match self {
            Download::File(url) => (*url).to_owned(),
            Download::Disks(disks) => match disks.split_first() {
                Some((first, [])) => first[0].to_owned(),
                Some((first, rest)) => format!("{} (+{} disks)", first[0], rest.len()),
                None => String::new(),
            },
        }
    }
}

/// The downloads of one release, in the order they are worth trying.
///
/// A `download` field mixes three things: the release itself, extras (music
/// rips, scans, ...), and alternative copies of the release — a mirror, a
/// reupload, the same demo packed as `.adf` and as `.dms`. Alternatives and the
/// disks of a set look alike and want opposite handling, so they are told apart
/// by [`disk_stem`]: same stem, same disk.
///
/// If any URL is a disk image the release is disk based, and those images make
/// up the first attempt, one disk per distinct stem. Everything that isn't a
/// disk image then follows as an attempt of its own, so a release whose disk
/// links have all died still loads from the `.zip` beside them.
///
/// Disk images are kept whatever their format, since the disks of one set may
/// well be archived differently: Hardwired by The Silents & Crionics has side A
/// as a `.dms` and side B as an `.adf`, and keying on the extension alone would
/// silently fetch only one of the two.
///
/// Known extras are left out: they are never the release, and loading the
/// soundtrack because the demo 404'd is worse than failing. Dropping everything
/// would leave nothing to fetch at all, so a filter that empties the list is
/// itself dropped and every URL becomes an attempt.
pub fn release_downloads<'a>(urls: &[&'a str]) -> Vec<Download<'a>> {
    /// Extensions that are never the main file of a release.
    const IGNORED_EXTENSIONS: [&str; 3] = ["sid", "pdf", "rtf"];

    let mut disks: Vec<(String, Vec<&'a str>)> = Vec::new();
    for url in urls.iter().copied().filter(|u| is_disk_image_url(u)) {
        let stem = disk_stem(url);
        match disks.iter_mut().find(|(s, _)| *s == stem) {
            Some((_, alternatives)) => alternatives.push(url),
            None => disks.push((stem, vec![url])),
        }
    }

    let mut downloads = Vec::new();
    if !disks.is_empty() {
        downloads.push(Download::Disks(
            disks.into_iter().map(|(_, disk)| disk).collect(),
        ));
    }
    downloads.extend(
        urls.iter()
            .copied()
            .filter(|u| !is_disk_image_url(u))
            .filter(|u| !url_extension(u).is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.as_str())))
            .map(Download::File),
    );
    if downloads.is_empty() {
        downloads = urls.iter().copied().map(Download::File).collect();
    }
    downloads
}

/// Download the first of `urls` that succeeds, using `fetch` for each attempt.
///
/// The URLs grouped together here are alternatives — a mirror, a reupload, the
/// same file under a second link class — so a link that is dead, 404s or times
/// out only rules out that one URL. Every failure is logged and the last one is
/// returned if none of them work; with nothing downloaded the caller has only
/// one failure to report, and the last is the one that ran out of alternatives.
///
/// Note that each URL is itself tried against every mirror its link class has
/// (see [`crate::fetch`]); reaching the next URL here means all of those failed.
///
/// `fetch` is a parameter so the walk can be tested without a network.
fn fetch_first_available(
    urls: &[impl AsRef<str>],
    on_progress: OnProgress<'_>,
    fetch: &mut impl FnMut(&str, OnProgress<'_>) -> Result<PathBuf>,
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

/// Fetch everything one [`Download`] needs, and report where it landed: the
/// file itself for a [`Download::File`], or a directory holding one file per
/// disk for a [`Download::Disks`].
///
/// A disk falls back through its own alternatives, but a disk that can't be
/// fetched at all fails the whole set: half a set won't boot, and the caller
/// has other downloads to fall back on.
fn fetch_download(
    download: &Download<'_>,
    on_progress: OnProgress<'_>,
    fetch: &mut impl FnMut(&str, OnProgress<'_>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    // Normalized rather than passed on as they are stored — see `cache_keys`
    // for why the cache wants that form.
    match download {
        Download::File(url) => fetch(&cache_key(url), on_progress),
        Download::Disks(disks) => {
            let mut files = Vec::with_capacity(disks.len());
            for alternatives in disks {
                // A disk set is several downloads in a row, so nothing is
                // reported: forwarding each one's byte count would restart the
                // progress bar on every disk.
                files.push(fetch_first_available(
                    &cache_keys(alternatives),
                    &|_, _| {},
                    fetch,
                )?);
            }
            crate::fetch::gather_files(&files)
        }
    }
}

/// Download the first of `downloads` that succeeds, using `fetch` for each URL.
///
/// The attempts [`release_downloads`] builds are alternative ways of getting
/// the same release, so a whole attempt failing — every disk alternative and
/// every mirror under it — only rules out that attempt. As in
/// [`fetch_first_available`], the last failure is the one reported.
fn fetch_release(
    downloads: &[Download<'_>],
    on_progress: OnProgress<'_>,
    mut fetch: impl FnMut(&str, OnProgress<'_>) -> Result<PathBuf>,
) -> Result<PathBuf> {
    let mut last_error = None;
    for download in downloads {
        match fetch_download(download, on_progress, &mut fetch) {
            Ok(path) => return Ok(path),
            Err(e) => {
                warn!("download failed for {}: {e:#}", download.label());
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
    urls.iter().copied().map(cache_key).collect()
}

/// One URL in that form; see [`cache_keys`].
fn cache_key(url: &str) -> String {
    Url::parse(url).map_or_else(|_| url.to_owned(), |parsed| parsed.to_string())
}

impl FileSource {
    /// Narrow a URL-backed source down to the one URL whose file name is
    /// `name`, for an [`Override`] that says which of a release's downloads is
    /// the demo.
    ///
    /// A demozoo release often lists the demo, its soundtrack and a scan of the
    /// disk label side by side, and [`release_downloads`] can only guess
    /// between them from the extensions. Naming the file settles it.
    ///
    /// A name that matches nothing leaves the list alone and warns: the entry
    /// still has its URLs, so the load falls back to guessing rather than
    /// failing outright — which is what happens when a mirror renames a file
    /// out from under an override written months ago.
    pub fn pick_download(&mut self, name: &str) {
        let FileSource::Url(urls) = self else {
            return;
        };
        let picked = urls
            .iter()
            .find(|url| url_file_name(url).is_some_and(|f| f.eq_ignore_ascii_case(name)));
        match picked {
            Some(url) => *urls = UrlList::one(url),
            None => warn!("No download named {name:?} among {:?}", urls.as_slice()),
        }
    }

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
            // The release's downloads as ways of getting at it, best first: a
            // disk set that sits together in one directory (built into an m3u
            // later), an archive, a reupload. The first that lands wins.
            let downloads = release_downloads(urls.as_slice());
            let p = fetch_release(&downloads, on_progress, fetch_url_with_progress)?;
            *self = FileSource::Path(p);
        }
        match self {
            FileSource::Path(p) => Ok(p),
            FileSource::Url(_) => unreachable!("just converted to Path above"),
        }
    }
}

// enum Rank {
//     Pouet,
//     SceneAwards,
//     Party,
//     Cdc,
//     Thumbs,
// }

#[derive(Debug, Copy, Clone, Default, PartialOrd, Ord, PartialEq, Eq)]
pub struct CompactDate(u32);
impl CompactDate {
    pub fn new(year: u32, month: u32, day: u32) -> Self {
        Self((year << 9) | (month << 5) | day)
    }
    pub fn parse(date: &str) -> Self {
        let date_s: Vec<&str> = date.split(['-', '/', '.']).collect();
        let year_s = date_s[0];
        let year = year_s.parse::<u32>().unwrap_or(0);
        let month = date_s.get(1).unwrap_or(&"0").parse::<u32>().unwrap_or(0);
        let day = date_s.get(2).unwrap_or(&"0").parse::<u32>().unwrap_or(0);
        Self::new(year, month, day)
    }
    pub fn year(&self) -> u32 {
        self.0 >> 9
    }
}

// CDC, Starred, Winner x ( Scene, Party) RunnerUp x (Scene Party)

/// The whole file list lives for the run (see [`EmuFile`]), so the strings here
/// are `&'static str` — either literals, slices of the leaked db text, or
/// individually leaked (see [`crate::files`]).
#[derive(Default, Debug, Clone, Copy)]
pub struct GameInfo {
    pub title: &'static str,
    pub group: &'static str,
    pub date: CompactDate,
    pub category: &'static str,
    pub rank: u32,
    // ..|Pp|Ss|*|cccccccc
    // awards: u32,
}

impl GameInfo {
    pub fn year(&self) -> u32 {
        self.date.year()
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

#[derive(Default, Debug, Clone)]
pub struct Patch {
    // File name of file to be patched
    pub target: &'static str,
    // Offset into file where data goes. None means replace entire file (normal case)
    pub offset: Option<usize>,
    // Data, base64 encoded
    pub data: &'static str,
    // Info to user
    pub info: &'static str,
}

impl Patch {
    /// The bytes to write, decoded from [`Self::data`].
    ///
    /// Kept encoded rather than decoded up front because that is the form the
    /// toml carries and the form the struct is built from; a patch is a config
    /// file of a few dozen bytes, so decoding it per load costs nothing.
    pub fn bytes(&self) -> Result<Vec<u8>> {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD
            .decode(self.data.trim())
            .with_context(|| format!("Bad base64 in patch for {:?}", self.target))
    }
}

/// A per-release fixup, read from `overrides.toml` and keyed on the demozoo id
/// of the release it is for — see [`crate::overrides`].
///
/// A release the db describes correctly needs none of this; these are for the
/// ones where the db's own answer is wrong or ambiguous — several downloads
/// where only one is the demo, an archive holding more than one program, a DOS
/// release whose sound config has to say GUS before it makes any noise.
#[derive(Default, Debug, Clone)]
pub struct Override {
    // If Some, select the URL ending with this file-name for download
    pub download: Option<&'static str>,
    // If Some, override file selection by system and pass this file directly to load()
    pub boot_file: Option<&'static str>,
    // Add this meta-data to WorkFile
    pub meta: HashMap<&'static str, &'static str>,
    // Patch these files after unpacking
    pub patches: Vec<Patch>,
    // Run the release on the fast Amiga configuration (`newsys::amiga::apply_fast`),
    // for the ones that need more machine than their year or tags suggest.
    pub fast: bool,
}

#[cfg(test)]
#[path = "tests/emu_file_tests.rs"]
mod tests;
