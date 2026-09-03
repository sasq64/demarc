use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Context;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tracing::{info, warn};
use url::Url;

use crate::cache::FileCache;

/// Give up after this many HTTP redirects, matching typical browser limits.
const MAX_REDIRECTS: usize = 10;

/// How long to wait for name resolution plus a connection before giving up on a
/// host. A scene archive that is up answers well inside this; one that is down
/// otherwise leaves the connect hanging until the OS gives up minutes later.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the server to start answering once connected — for HTTP
/// that is the response head, for FTP a reply on the control connection.
///
/// This deliberately does *not* bound the transfer itself: large demo archives
/// off a slow mirror are normal and must not be cut off mid-download. It only
/// bounds the part where a wedged server has told us nothing at all.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);

/// Where the download cache stops being small files and starts being big ones.
///
/// A megabyte is comfortably above a demo, a tune or a cracktro and comfortably
/// below a disk image or a CD track, which is what the split is for: the two
/// kinds are downloaded in wildly different numbers and cost wildly different
/// amounts to fetch again.
const SMALL_FILE: u64 = 1024 * 1024;

/// How large the small half of the download cache is allowed to grow before
/// [`prune_cache`] starts evicting from it. Demos and small archives are tiny
/// individually but unbounded in number, so without a cap a long-running
/// collection browse just keeps filling the disk — and thousands of them would,
/// out of one shared budget, evict every big download there was.
const SMALL_LIMIT: u64 = 250 * 1024 * 1024;

/// The same, for everything above [`SMALL_FILE`]: fewer entries, each of them
/// minutes of downloading to get back, so they get the larger share.
///
/// Both are defaults: the cache writes them to a `.limit` file the user can
/// edit.
const LARGE_LIMIT: u64 = 750 * 1024 * 1024;

/// Where each archive named by a db `download:` field lives, best mirror first.
///
/// Demozoo does not store a url for the files it knows an archive for: it
/// stores a *link class* plus a parameter, and the db generator keeps that pair
/// as `SceneOrgFile:/parties/2006/assembly06/demo/x.zip` rather than resolving
/// it (see demodb's demozoo.py). Resolving it here instead means a mirror that
/// dies — or a faster one appearing — is a change to this table rather than a
/// regenerate of every db file that mentions it.
///
/// The url is simply the base with the parameter appended, so a base carries
/// whatever trailing `/` or `?` the join needs. A download walks the mirrors in
/// order and keeps the first that answers; which one it starts from is
/// [`MIRROR_ROTATION`], so the list here is the order to prefer when every
/// mirror is up.
///
/// Any class listed here is also a scheme demarc will accept as a url, so keep
/// the names distinct from real schemes. Matching is case-insensitive: the db
/// spells the class the way Demozoo does, but a value that has been through
/// `Url::parse` (which is how db lines reach [`fetch_url`]) arrives lowercased.
const LINK_BASES: &[(&str, &[&str])] = &[
    (
        "AmigascneFile",
        &[
            "https://files.scene.org/get:fi-ftp/mirrors/amigascne",
            "https://files.scene.org/get:de-https/mirrors/amigascne",
            "ftp://ftp.amigascne.org/pub/amiga",
        ],
    ),
    (
        "SceneOrgFile",
        &[
            // The bare `/get/` link 302-redirects to a slow FTP mirror, so name
            // a mirror directly. (`URL_REWRITES` below rewrites `/get/` the same
            // way, for the plain urls a db holds for the same files.)
            "https://files.scene.org/get:de-https",
            "https://files.scene.org/get:fi-ftp",
        ],
    ),
    ("ModlandFile", &["https://ftp.modland.com"]),
    (
        "FujiologyFile",
        &["https://ftp.untergrund.net/users/ltk_tscc/fujiology"],
    ),
    ("UntergrundFile", &["https://ftp.untergrund.net"]),
    ("PaduaOrgFile", &["http://ftp.padua.org/pub/c64"]),
    ("Defacto2File", &["https://defacto2.net/f/"]),
    ("ModarchiveModule", &["https://modarchive.org/module.php?"]),
    ("SixteenColorsPack", &["https://16colo.rs/pack/"]),
];

/// Fixups applied to every resolved url, as `(prefix, replacement)` pairs; the
/// first matching rule wins.
///
/// These are links that are correct as a db records them but not as written
/// downloadable: they point at a redirect, a dead host name, or a doubled path
/// prefix. Rules match the url *after* [`LINK_BASES`] has been applied, so a
/// rule must not undo a mirror choice made there — hence no rule for the bases
/// listed above.
///
///   funet, sndh  plain http (or ftp) no longer serves these files.
///   scene.org    a `/get/` link 302-redirects to a slow FTP mirror; the
///                `/get:de-https/` variant serves the file over HTTPS directly.
///   modland      some links already carry the `/pub/modules` prefix, giving a
///                doubled path once the base is prepended.
///   untergrund   the fujiology archive moved from user ltk_tscl to ltk_tscc.
const URL_REWRITES: &[(&str, &str)] = &[
    ("ftp://ftp.funet.fi/", "https://ftp.funet.fi/"),
    (
        "https://files.scene.org/get/",
        "https://files.scene.org/get:de-https/",
    ),
    (
        "https://ftp.modland.com/pub/modules/pub/modules/",
        "https://ftp.modland.com/pub/modules/",
    ),
    (
        "https://ftp.untergrund.net/users/ltk_tscl/",
        "https://ftp.untergrund.net/users/ltk_tscc/",
    ),
    ("http://sndh.atari.org/", "https://sndh.atari.org/"),
];

/// Which mirror each [`LINK_BASES`] class starts from, as an index into that
/// class's list; the rest of the list follows it cyclically. A download that
/// had to fall past a dead or slow mirror records the one that actually worked
/// here, so later downloads of the same class start where the last success was
/// instead of timing out against the same broken host every time.
///
/// One entry per class, in [`LINK_BASES`] order. Memory only: a fresh run
/// starts from the order the table is written in.
static MIRROR_ROTATION: LazyLock<Vec<AtomicUsize>> =
    LazyLock::new(|| LINK_BASES.iter().map(|_| AtomicUsize::new(0)).collect());

/// One mirror, as a pair of indices into [`LINK_BASES`].
#[derive(Clone, Copy)]
struct Mirror {
    class: usize,
    base: usize,
}

/// One url a download can be attempted against, plus the mirror it came from
/// when it was built from a link class — that is what a successful attempt
/// feeds back into [`MIRROR_ROTATION`].
struct Candidate {
    url: String,
    mirror: Option<Mirror>,
}

/// The [`LINK_BASES`] entry and the parameter to append to its mirrors, if `s`
/// names a link class rather than spelling a url out.
fn link_class(s: &str) -> Option<(usize, &str)> {
    let (class, parameter) = s.split_once(':')?;
    let index = LINK_BASES
        .iter()
        .position(|(name, _)| class.eq_ignore_ascii_case(name))?;
    Some((index, parameter))
}

/// Every url `s` could be downloaded from, best first: a link class's mirrors
/// starting at the one [`MIRROR_ROTATION`] currently prefers, or just `s`
/// itself for a url that is already spelled out.
fn candidates(s: &str) -> Vec<Candidate> {
    let Some((class, parameter)) = link_class(s) else {
        return vec![Candidate {
            url: rewrite(s),
            mirror: None,
        }];
    };
    let bases = LINK_BASES[class].1;
    let start = MIRROR_ROTATION[class].load(Ordering::Relaxed);
    (0..bases.len())
        .map(|offset| (start + offset) % bases.len())
        .map(|base| Candidate {
            url: rewrite(&format!("{}{parameter}", bases[base])),
            mirror: Some(Mirror { class, base }),
        })
        .collect()
}

/// Remember `mirror` as the one that worked, rotating its class's list so the
/// next download of that class starts there.
///
/// The winner is stored as an absolute index rather than as "rotate past the
/// mirrors we skipped", so a download finishing while another thread rotates
/// the same class still leaves the list pointing at a mirror known to answer.
fn promote_mirror(mirror: Mirror) {
    MIRROR_ROTATION[mirror.class].store(mirror.base, Ordering::Relaxed);
}

/// Apply the first matching [`URL_REWRITES`] rule to `url`.
fn rewrite(url: &str) -> String {
    for (prefix, replacement) in URL_REWRITES {
        if let Some(rest) = url.strip_prefix(prefix) {
            return format!("{replacement}{rest}");
        }
    }
    url.to_string()
}

/// The urls of [`candidates`], best first — the [`LINK_BASES`] mirrors for a
/// link class, or just `s` itself for a url that is already spelled out. Either
/// way the result has been through [`rewrite`].
pub fn resolve_url(s: &str) -> Vec<String> {
    candidates(s).into_iter().map(|c| c.url).collect()
}

/// The url a download starts at: the currently preferred mirror of a link
/// class, or the url as given. A link class with no mirrors at all falls back
/// to the input, which then fails at download rather than here.
fn primary_url(s: &str) -> String {
    resolve_url(s)
        .into_iter()
        .next()
        .unwrap_or_else(|| s.into())
}

/// True if `s` looks like a remote URL demarc should download rather than treat
/// as a local path — either a downloadable scheme or a [`LINK_BASES`] class.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("ftp://")
        || link_class(s).is_some()
}

/// Download the file at `url` into a local cache directory and return its path.
///
/// Files are cached under `<cache>/demarc/downloads/<url-hash>/<name>`, so
/// re-opening the same link reuses the existing download. The hash covers the
/// whole URL while the leaf keeps its readable, correctly-suffixed name (see
/// [`crate::cache::FileCache::get_file`] and [`url_filename`]) — downstream
/// dispatch keys on the file extension, so the extension has to survive. The
/// download goes to a `.part` file that is renamed into place on success, so an
/// interrupted transfer never leaves a truncated file masquerading as a valid
/// cache hit.
pub fn fetch_url(url: &str) -> anyhow::Result<PathBuf> {
    fetch_url_with_progress(url, &|_, _| {})
}

/// Reports `(bytes written so far, total size if the server declared one)` as a
/// download runs. Called once per write, so on every chunk `std::io::copy`
/// moves — cheap enough for an atomic store, too often for anything expensive.
pub type OnProgress<'a> = &'a (dyn Fn(u64, Option<u64>) + Send + Sync);

/// [`fetch_url`] with progress reporting, for callers that can display it (see
/// [`crate::jobs::Jobs::download`]).
///
/// `on_progress` is not called at all for a cache hit — there is nothing to
/// download — so a progress bar should not assume it will ever fire.
pub fn fetch_url_with_progress(url: &str, on_progress: OnProgress<'_>) -> anyhow::Result<PathBuf> {
    // The cache entry is keyed on the url as the db writes it, so a link class
    // keeps its cache entry when [`LINK_BASES`] changes mirror; the name inside
    // it comes from the resolved url, which is the one that carries the file's
    // real name and extension.
    let name = url_filename(&primary_url(url));
    DOWNLOADS.get_file(url, &name, |dest| download_to(url, dest, on_progress))
}

/// Copy `files` into a single fresh temp directory and return that directory's
/// path, so the disks of a set end up side by side in one directory.
///
/// The originals stay where they are — these are copies of cache entries, made
/// because a disk set has to be one directory and the cache stores one entry
/// per URL. Each copy keeps the cached file's URL-derived name (see
/// [`url_filename`]), which is what ends up in the generated m3u, so two disks
/// of one set whose URLs differ only in a directory would land on the same name
/// here — they stay apart in the cache, but the copy below still flattens them.
pub fn gather_files(files: &[PathBuf]) -> anyhow::Result<PathBuf> {
    let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
    for file in files {
        let name = file
            .file_name()
            .with_context(|| format!("cached download has no filename: {}", file.display()))?;
        std::fs::copy(file, dir.join(name))?;
    }
    Ok(dir)
}

/// The download cache: one entry per URL hash, each holding the downloaded file
/// under its URL-derived name, budgeted separately by size (see
/// [`SMALL_FILE`]).
static DOWNLOADS: LazyLock<FileCache> =
    LazyLock::new(|| FileCache::new("downloads", LARGE_LIMIT).with_level(SMALL_FILE, SMALL_LIMIT));

/// Trim the download cache back under its budgets ([`SMALL_LIMIT`] and
/// [`LARGE_LIMIT`], or whatever the cache's `.limit` says). Intended to run
/// once at startup, when nothing is holding a path into it yet.
pub fn prune_cache() {
    DOWNLOADS.prune();
}

/// Download `url` to `path`, trying every url [`candidates`] offers for it
/// until one works — for a link class that is each mirror in turn, and the one
/// that answered is remembered for next time (see [`MIRROR_ROTATION`]).
///
/// `path` is the staging file [`crate::cache::FileCache`] hands out, which it
/// publishes under the entry's real name only once this returns `Ok` — so an
/// interrupted download never leaves a truncated file masquerading as a
/// complete one. Each attempt truncates it afresh, so a mirror that died
/// mid-transfer leaves nothing behind for the next one to append to.
/// `on_progress` restarts from zero on each attempt, which is honest: that
/// transfer really is starting over somewhere else.
///
/// Every failure is logged and the last one is returned if nothing works, as
/// the one that ran out of alternatives.
fn download_to(url: &str, path: &Path, on_progress: OnProgress<'_>) -> anyhow::Result<()> {
    let mut last_error = None;
    for candidate in candidates(url) {
        match download_part(&candidate.url, path, on_progress) {
            Ok(()) => {
                if let Some(mirror) = candidate.mirror {
                    promote_mirror(mirror);
                }
                return Ok(());
            }
            Err(e) => {
                warn!("download failed for {}: {e:#}", candidate.url);
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no download url for {url}")))
}

/// One [`download_to`] attempt: `path` is created from scratch, so whatever a
/// previous mirror managed to write into it is discarded.
fn download_part(url: &str, path: &Path, on_progress: OnProgress<'_>) -> anyhow::Result<()> {
    let mut file = std::fs::File::create(path)?;
    download(url, &mut file, on_progress)?;
    file.flush()?;
    Ok(())
}

/// Download `url` (`http`, `https` or `ftp`) into `out`.
///
/// HTTP redirects are followed manually rather than by `ureq` so that a
/// redirect from an `http(s)://` URL to an `ftp://` one — as files.scene.org
/// does for its `/get/...` download links — is handled by switching to the FTP
/// transport instead of failing on the unknown scheme.
///
/// `url` is a url proper: resolving a db's `LinkClass:parameter` pair to the
/// mirrors to try, and the fixups for links that no longer work as recorded,
/// happen a level up in [`download_to`].
fn download(url: &str, out: &mut impl Write, on_progress: OnProgress<'_>) -> anyhow::Result<()> {
    info!("Downloading {url}...");
    if url.starts_with("ftp://") {
        return fetch_ftp(url, out, on_progress);
    }

    let mut current = url.to_string();
    for _ in 0..MAX_REDIRECTS {
        let response = ureq::get(&current)
            .config()
            .max_redirects(0)
            .timeout_resolve(Some(CONNECT_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .build()
            .call()
            .context(format!("{url:?} failed"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get("location")
                .context("redirect response is missing a Location header")?
                .to_str()
                .context("redirect Location header is not valid text")?;
            let next = Url::parse(&current)
                .and_then(|base| base.join(location))
                .with_context(|| format!("invalid redirect target: {location}"))?;
            if next.scheme() == "ftp" {
                return fetch_ftp(next.as_str(), out, on_progress);
            }
            info!("Redirected to {next}");
            current = next.into();
            continue;
        }
        // `Content-Length` is the transfer size, which is the file size only
        // when the body isn't compressed. Scene archives are already-compressed
        // binaries that servers hand over as-is, so in practice it matches; if
        // one ever does gzip a response the bar just tops out early.
        let total = response
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let mut reader = response.into_body().into_reader();
        let mut out = CountingWriter::new(out, total, on_progress);
        std::io::copy(&mut reader, &mut out)?;
        return Ok(());
    }
    anyhow::bail!("too many redirects while fetching {url}");
}

/// Download an `ftp://` URL into `out`.
///
/// Supports an optional `user:password@` prefix in the authority; without one it
/// logs in anonymously. Transfers are done in binary mode so files aren't
/// corrupted by line-ending translation.
///
/// The path is percent-decoded before it goes out as the `RETR` argument: FTP
/// has no percent-encoding, so a server asked for `Count%20Duckula.png` looks
/// for a file with a literal `%20` in its name and answers 550. URLs reach here
/// encoded either because the db has them that way or because `Url::join`
/// encoded a redirect `Location` that contained raw spaces — which is exactly
/// what files.scene.org's `/get/...` links redirect to.
fn fetch_ftp(url: &str, out: &mut impl Write, on_progress: OnProgress<'_>) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    use suppaftp::FtpStream;
    use suppaftp::types::FileType;

    let rest = url.strip_prefix("ftp://").context("not an ftp URL")?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    anyhow::ensure!(!path.is_empty(), "ftp URL has no file path: {url}");
    let path = percent_decode_str(path).decode_utf8_lossy();

    let (credentials, host_port) = match authority.rsplit_once('@') {
        Some((creds, host)) => (Some(creds), host),
        None => (None, authority),
    };
    let (user, pass) = match credentials {
        Some(creds) => match creds.split_once(':') {
            Some((u, p)) => (u.to_string(), p.to_string()),
            None => (creds.to_string(), String::new()),
        },
        None => ("anonymous".to_string(), "anonymous@".to_string()),
    };
    let host_port = if host_port.contains(':') {
        host_port.to_string()
    } else {
        format!("{host_port}:21")
    };

    // Connect with an explicit timeout rather than `FtpStream::connect`, which
    // has none and so hangs for the OS default on a dead host. That needs a
    // resolved `SocketAddr`, so do the DNS lookup here and take the first
    // address the resolver hands back.
    let addr = host_port
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve {host_port}"))?
        .next()
        .with_context(|| format!("{host_port} resolved to no addresses"))?;
    let mut ftp = FtpStream::connect_timeout(addr, CONNECT_TIMEOUT)
        .with_context(|| format!("failed to connect to {host_port}"))?;
    // Bound waits on the *control* connection only; the data connection used by
    // `retr` below is a separate socket, so a large slow transfer is unaffected.
    let _ = ftp.get_ref().set_read_timeout(Some(RESPONSE_TIMEOUT));
    let _ = ftp.get_ref().set_write_timeout(Some(RESPONSE_TIMEOUT));
    ftp.login(&user, &pass).context("FTP login failed")?;
    ftp.transfer_type(FileType::Binary)?;
    // Not every server implements SIZE; without it the download just reports an
    // unknown total and progress stays indeterminate.
    let total = ftp.size(&path).ok().map(|size| size as u64);
    let mut out = CountingWriter::new(out, total, on_progress);
    ftp.retr(&path, |reader| {
        std::io::copy(reader, &mut out).map_err(suppaftp::FtpError::ConnectionError)?;
        Ok(())
    })
    .with_context(|| format!("failed to retrieve {path}"))?;
    let _ = ftp.quit();
    Ok(())
}

/// Wraps the download's output and reports the running byte count through an
/// [`OnProgress`] callback, so the transfer loop stays plain `std::io::copy`.
struct CountingWriter<'a, W> {
    inner: W,
    done: u64,
    total: Option<u64>,
    on_progress: OnProgress<'a>,
}

impl<'a, W: Write> CountingWriter<'a, W> {
    fn new(inner: W, total: Option<u64>, on_progress: OnProgress<'a>) -> Self {
        Self {
            inner,
            done: 0,
            total,
            on_progress,
        }
    }
}

impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.done += written as u64;
        (self.on_progress)(self.done, self.total);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Everything outside the URL-unreserved set gets percent-encoded, which also
/// happens to be exactly the set of characters safe in a filename.
const FILENAME_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Derive a filesystem-safe filename from a URL's final path segment, dropping
/// any `?query` or `#fragment` and percent-encoding anything that isn't an
/// unreserved character so the result is safe to use as a cache key.
///
/// The segment is percent-*decoded* first so an already-encoded URL doesn't
/// come back doubly encoded — `Count%20Duckula.png` is a space, not a literal
/// `%20`, and re-encoding the `%` would name the cached file
/// `Count%2520Duckula.png`.
fn url_filename(url: &str) -> String {
    let tail = url.rsplit_once('/').map_or(url, |(_, tail)| tail);
    let tail = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    let tail = percent_decode_str(tail).decode_utf8_lossy();
    let cleaned = utf8_percent_encode(&tail, FILENAME_ESCAPE).to_string();
    if cleaned.is_empty() || cleaned == "." {
        "download".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
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
}
