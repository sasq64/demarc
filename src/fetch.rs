//! Downloading of remote files so demarc can be launched with an `http(s)://`
//! or `ftp://` URL (e.g. from a browser's "Open with" context menu) instead of
//! a local path.

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use tracing::info;
use url::Url;

/// Give up after this many HTTP redirects, matching typical browser limits.
const MAX_REDIRECTS: usize = 10;

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
const URL_REWRITES: &[(&str, &str)] = &[(
    "https://files.scene.org/get/*",
    "https://files.scene.org/get:de-https/*",
)];

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
/// Files are cached under `<cache>/demarc/downloads/`, keyed by the URL's final
/// path segment, so re-opening the same link reuses the existing download. The
/// download goes to a `.part` temp file that is renamed into place on success,
/// so an interrupted transfer never leaves a truncated file masquerading as a
/// valid cache hit.
pub fn fetch_url(url: &str) -> anyhow::Result<PathBuf> {
    let name = url_filename(url);
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("demarc")
        .join("downloads");
    std::fs::create_dir_all(&dir)?;

    let path = dir.join(&name);
    if path.is_file() {
        return Ok(path);
    }

    info!("Downloading {url}...");
    let tmp = dir.join(format!(".{name}.part"));
    let mut file = std::fs::File::create(&tmp)?;
    download(url, &mut file)?;
    file.flush()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
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

    let mut ftp = FtpStream::connect(&host_port)
        .with_context(|| format!("failed to connect to {host_port}"))?;
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

/// Derive a filesystem-safe filename from a URL's final path segment, dropping
/// any `?query` or `#fragment` and replacing anything that isn't an
/// alphanumeric or `. _ -` so the result is safe to use as a cache key.
fn url_filename(url: &str) -> String {
    let tail = url.rsplit('/').next().unwrap_or("download");
    let tail = tail.split(['?', '#']).next().unwrap_or(tail);
    let cleaned: String = tail
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
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
    fn extracts_filename() {
        assert_eq!(url_filename("https://x.com/path/foo.zip"), "foo.zip");
        assert_eq!(url_filename("https://x.com/path/foo.zip?a=b"), "foo.zip");
        assert_eq!(url_filename("https://x.com/foo%20bar.d64"), "foo_20bar.d64");
        assert_eq!(url_filename("https://x.com/"), "download");
    }
}
