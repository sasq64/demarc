use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use sha2::{Digest, Sha256};
use tracing::info;
use url::Url;

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

/// How large the download cache is allowed to grow before [`prune_cache`]
/// starts evicting from it. Demo and game archives are small individually but
/// unbounded in number, so without a cap a long-running collection browse just
/// keeps filling the disk.
const CACHE_LIMIT: u64 = 500 * 1024 * 1024;

/// True if `s` looks like a remote URL demarc should download rather than treat
/// as a local path.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ftp://")
}

/// URL rewrite rules applied before downloading, as `(pattern, replacement)`
/// pairs. A trailing `*` in the pattern matches any suffix, which is then
/// substituted for the `*` in the replacement.
///
/// The scene.org rule turns a `/get/` link — which 302-redirects to a slow FTP
/// mirror — into its `/get:de-https/` variant, which serves the file directly
/// over HTTPS.
const URL_REWRITES: &[(&str, &str)] = &[
    (
        "https://files.scene.org/get/*",
        "https://files.scene.org/get:de-https/*",
    ),
    (
        "https://ftp.untergrund.net/users/ltk_tscl/fujiology/*",
        "https://fujiology.org/*",
    ),
];

/// Rewrite `url` according to the first matching rule in [`URL_REWRITES`],
/// returning it unchanged if no rule applies.
pub fn translate_url(url: &str) -> String {
    for (pattern, replacement) in URL_REWRITES {
        if let Some(prefix) = pattern.strip_suffix('*')
            && let Some(rest) = url.strip_prefix(prefix)
            && let Some(repl_prefix) = replacement.strip_suffix('*')
        {
            return format!("{repl_prefix}{rest}");
        }
    }
    url.to_string()
}

/// Download the file at `url` into a local cache directory and return its path.
///
/// Files are cached under `<cache>/demarc/downloads/<url-hash>/<name>`, so
/// re-opening the same link reuses the existing download. The hash covers the
/// whole URL while the leaf keeps its readable, correctly-suffixed name (see
/// [`url_hash`] and [`url_filename`]) — downstream dispatch keys on the file
/// extension, so the extension has to survive. The download goes to a `.part`
/// temp file that is renamed into place on success, so an interrupted transfer
/// never leaves a truncated file masquerading as a valid cache hit.
pub fn fetch_url(url: &str) -> anyhow::Result<PathBuf> {
    let name = url_filename(url);
    let dir = downloads_dir().join(url_hash(url));
    std::fs::create_dir_all(&dir)?;

    let path = dir.join(&name);
    if path.is_file() {
        // Mark the hit as recent so [`prune_cache`] evicts genuinely unused
        // downloads rather than merely old ones.
        touch(&path);
        return Ok(path);
    }

    info!("Downloading {url}...");
    download_to(url, &path)?;
    Ok(path)
}

/// Gather several URLs into a single fresh temp directory and return that
/// directory's path, so multi-disk sets end up side by side in one directory.
///
/// Each URL is fetched through [`fetch_url`], so it is cached individually; when
/// they are all already cached this just copies the cached files across without
/// re-downloading. Each file keeps its URL-derived name (see [`url_filename`]),
/// which is what ends up in the generated m3u, so two disks of one set whose
/// URLs differ only in a directory would land on the same name here — they stay
/// apart in the cache, but the copy below still flattens them.
pub fn fetch_urls(urls: &[Url]) -> anyhow::Result<PathBuf> {
    let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
    for url in urls {
        let cached = fetch_url(url.as_ref())?;
        let name = cached
            .file_name()
            .with_context(|| format!("cached download has no filename: {}", cached.display()))?;
        std::fs::copy(&cached, dir.join(name))?;
    }
    Ok(dir)
}

/// Root of the download cache: one subdirectory per URL hash, each holding the
/// downloaded file under its URL-derived name.
fn downloads_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("demarc")
        .join("downloads")
}

/// Set `path`'s access and modification times to now, recording it as recently
/// used for [`prune_cache`].
///
/// Failures are ignored: a cache entry we can't touch is not worth failing a
/// download over, it just risks being evicted earlier than it should be.
fn touch(path: &Path) {
    let now = SystemTime::now();
    let times = std::fs::FileTimes::new().set_accessed(now).set_modified(now);
    // Opening for write, not read: on Windows `set_times` needs write access,
    // and on Unix futimens wants a handle we're allowed to modify.
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_times(times);
    }
}

/// Delete least-recently-used entries from the download cache until its total
/// size is back under [`CACHE_LIMIT`]. Intended to run once at startup, when
/// nothing is holding a path into the cache yet.
///
/// Eviction is per URL-hash directory — the unit a [`fetch_url`] cache hit is
/// keyed on — using the newest mtime inside it as its last-use time, which
/// [`fetch_url`] refreshes on every hit. Errors are logged and skipped rather
/// than propagated: a cache that can't be pruned is a disk-space problem, not a
/// reason to refuse to start.
pub fn prune_cache() {
    prune_dir(&downloads_dir(), CACHE_LIMIT);
}

/// [`prune_cache`] against an explicit directory and limit.
fn prune_dir(dir: &Path, limit: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        // No cache directory yet — nothing to prune.
        return;
    };

    let mut total = 0u64;
    let mut items: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let (size, used) = entry_stats(&path);
        total += size;
        items.push((used, size, path));
    }
    if total <= limit {
        return;
    }

    // Oldest first, so the entries nobody has opened in the longest go first.
    items.sort_by_key(|(used, ..)| *used);
    let mut freed = 0u64;
    let mut removed = 0usize;
    for (_, size, path) in items {
        if total <= limit {
            break;
        }
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => {
                total -= size;
                freed += size;
                removed += 1;
            }
            Err(e) => tracing::warn!("Failed to prune {}: {e}", path.display()),
        }
    }
    if removed > 0 {
        info!(
            "Pruned {removed} cached download(s), freeing {} MB",
            freed / (1024 * 1024)
        );
    }
}

/// Total size of `path` and the time it was last used, taken as the newest
/// mtime found within it. A path we can't stat counts as zero-sized and
/// last used at the epoch, so a broken entry is evicted first.
fn entry_stats(path: &Path) -> (u64, SystemTime) {
    let Ok(meta) = std::fs::metadata(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    if !meta.is_dir() {
        return (meta.len(), meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, SystemTime::UNIX_EPOCH);
    };
    let mut size = 0;
    let mut used = SystemTime::UNIX_EPOCH;
    for entry in entries.flatten() {
        let (child_size, child_used) = entry_stats(&entry.path());
        size += child_size;
        used = used.max(child_used);
    }
    (size, used)
}

/// Download `url` to `path`, writing first to a sibling `.part` file that is
/// renamed into place on success so an interrupted transfer never leaves a
/// truncated file masquerading as a complete one.
fn download_to(url: &str, path: &Path) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("download");
    let tmp = path.with_file_name(format!(".{name}.part"));
    let mut file = std::fs::File::create(&tmp)?;
    download(url, &mut file)?;
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Download `url` (`http`, `https` or `ftp`) into `out`.
///
/// HTTP redirects are followed manually rather than by `ureq` so that a
/// redirect from an `http(s)://` URL to an `ftp://` one — as files.scene.org
/// does for its `/get/...` download links — is handled by switching to the FTP
/// transport instead of failing on the unknown scheme.
fn download(url: &str, out: &mut impl Write) -> anyhow::Result<()> {
    let url = translate_url(url);
    let url = url.as_str();
    if url.starts_with("ftp://") {
        return fetch_ftp(url, out);
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
            .call()?;
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
                return fetch_ftp(next.as_str(), out);
            }
            info!("Redirected to {next}");
            current = next.into();
            continue;
        }
        let mut reader = response.into_body().into_reader();
        std::io::copy(&mut reader, out)?;
        return Ok(());
    }
    anyhow::bail!("too many redirects while fetching {url}");
}

/// Download an `ftp://` URL into `out`.
///
/// Supports an optional `user:password@` prefix in the authority; without one it
/// logs in anonymously. Transfers are done in binary mode so files aren't
/// corrupted by line-ending translation.
fn fetch_ftp(url: &str, out: &mut impl Write) -> anyhow::Result<()> {
    use std::net::ToSocketAddrs;

    use suppaftp::FtpStream;
    use suppaftp::types::FileType;

    let rest = url.strip_prefix("ftp://").context("not an ftp URL")?;
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    anyhow::ensure!(!path.is_empty(), "ftp URL has no file path: {url}");

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
    ftp.retr(path, |reader| {
        std::io::copy(reader, out).map_err(suppaftp::FtpError::ConnectionError)?;
        Ok(())
    })
    .with_context(|| format!("failed to retrieve {path}"))?;
    let _ = ftp.quit();
    Ok(())
}

/// Hash the whole URL into a hex string used as its cache subdirectory.
///
/// The last path segment alone is not a safe cache key: `.../v1/game.zip` and
/// `.../v2/game.zip` share one, so the second URL would silently be served the
/// first one's bytes. Keying the *directory* on the full URL keeps downloads
/// distinct while leaving the filename inside it readable and correctly
/// suffixed. 16 hex chars (64 bits) is far past any plausible collision here,
/// and SHA-256 keeps the mapping stable across toolchain upgrades so an
/// existing cache stays valid.
fn url_hash(url: &str) -> String {
    Sha256::digest(url.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
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
fn url_filename(url: &str) -> String {
    let tail = url.rsplit_once('/').map_or(url, |(_, tail)| tail);
    let tail = &tail[..tail.find(['?', '#']).unwrap_or(tail.len())];
    let cleaned = utf8_percent_encode(tail, FILENAME_ESCAPE).to_string();
    if cleaned.is_empty() || cleaned == "." {
        "download".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_urls() {
        assert!(is_url("https://example.com/a.zip"));
        assert!(is_url("http://example.com/a.zip"));
        assert!(is_url("ftp://example.com/a.zip"));
        assert!(!is_url("/home/user/a.zip"));
        assert!(!is_url("a.zip"));
    }

    #[test]
    fn translates_scene_org_urls() {
        assert_eq!(
            translate_url("https://files.scene.org/get/demos/groups/x/y.zip"),
            "https://files.scene.org/get:de-https/demos/groups/x/y.zip"
        );
        // Non-matching URLs pass through untouched.
        assert_eq!(
            translate_url("https://example.com/get/foo.zip"),
            "https://example.com/get/foo.zip"
        );
    }

    #[test]
    #[ignore = "hits the network"]
    fn downloads_https_to_ftp_redirect() {
        let mut buf = Vec::new();
        download(
            "https://files.scene.org/get/demos/groups/dual_crew_shining/gbc/dcs-nmod.zip",
            &mut buf,
        )
        .unwrap();
        assert_eq!(buf.len(), 46596);
        assert_eq!(&buf[..2], b"PK");
    }

    #[test]
    #[ignore = "hits the network"]
    fn caches_under_url_hash() {
        let url = "https://files.scene.org/get/demos/groups/dual_crew_shining/gbc/dcs-nmod.zip";
        let path = fetch_url(url).unwrap();
        assert_eq!(path.file_name().unwrap(), "dcs-nmod.zip");
        assert_eq!(path.parent().unwrap().file_name().unwrap(), &*url_hash(url));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 46596);
        // Second call is a cache hit on the same path, no re-download.
        assert_eq!(fetch_url(url).unwrap(), path);
    }

    #[test]
    fn extracts_filename() {
        assert_eq!(url_filename("https://x.com/path/foo.zip"), "foo.zip");
        assert_eq!(url_filename("https://x.com/path/foo.zip?a=b"), "foo.zip");
        // The `%` of an already-encoded segment is itself encoded, keeping the
        // mapping from URL to cache name unambiguous.
        assert_eq!(
            url_filename("https://x.com/foo%20bar.d64"),
            "foo%2520bar.d64"
        );
        assert_eq!(url_filename("https://x.com/a b&c.zip"), "a%20b%26c.zip");
        assert_eq!(url_filename("https://x.com/"), "download");
        assert_eq!(url_filename("game.zip"), "game.zip");
    }

    #[test]
    fn sums_size_and_newest_mtime_of_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("abc123");
        std::fs::create_dir(&entry).unwrap();
        std::fs::write(entry.join("a.zip"), vec![0u8; 100]).unwrap();
        std::fs::write(entry.join("b.zip"), vec![0u8; 200]).unwrap();

        // Age one file; the entry's last-use time must follow the *newest*
        // file in it, which `touch` here makes `b.zip`.
        let old = SystemTime::now() - Duration::from_secs(3600);
        let file = std::fs::File::options()
            .write(true)
            .open(entry.join("a.zip"))
            .unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        touch(&entry.join("b.zip"));

        let (size, used) = entry_stats(&entry);
        assert_eq!(size, 300);
        assert!(used > old);
    }

    #[test]
    fn prunes_least_recently_used_until_under_limit() {
        let cache = tempfile::tempdir().unwrap();
        // Three 100-byte entries, aged 3h / 2h / 1h ago.
        for (name, hours) in [("old", 3), ("mid", 2), ("new", 1)] {
            let entry = cache.path().join(name);
            std::fs::create_dir(&entry).unwrap();
            let path = entry.join("a.zip");
            std::fs::write(&path, vec![0u8; 100]).unwrap();
            let when = SystemTime::now() - Duration::from_secs(hours * 3600);
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(when))
                .unwrap();
        }

        // Under the limit: nothing is touched.
        prune_dir(cache.path(), 300);
        assert!(cache.path().join("old").exists());

        // Over it: evict oldest first, and stop as soon as we're back under.
        prune_dir(cache.path(), 150);
        assert!(!cache.path().join("old").exists());
        assert!(!cache.path().join("mid").exists());
        assert!(cache.path().join("new").exists());
    }

    #[test]
    fn touch_marks_a_file_as_recently_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.zip");
        std::fs::write(&path, b"x").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        drop(file);

        let (_, before) = entry_stats(&path);
        touch(&path);
        let (_, after) = entry_stats(&path);
        assert!(after > before);
    }

    #[test]
    fn hashes_whole_url() {
        // URLs sharing a final segment must not share a cache directory.
        assert_ne!(
            url_hash("https://x.com/v1/game.zip"),
            url_hash("https://x.com/v2/game.zip")
        );
        // ...but the same URL must always land on the same one.
        assert_eq!(
            url_hash("https://x.com/v1/game.zip"),
            url_hash("https://x.com/v1/game.zip")
        );
        assert_eq!(url_hash("https://x.com/v1/game.zip").len(), 16);
    }
}
