//! Downloading of remote files so demarc can be launched with an `http(s)://`
//! URL (e.g. from a browser's "Open with" context menu) instead of a local path.

use std::path::PathBuf;

/// True if `s` looks like an HTTP(S) URL demarc should download rather than
/// treat as a local path.
pub fn is_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
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

    println!("Downloading {url}...");
    let mut reader = ureq::get(url).call()?.into_body().into_reader();
    let tmp = dir.join(format!(".{name}.part"));
    let mut file = std::fs::File::create(&tmp)?;
    std::io::copy(&mut reader, &mut file)?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    Ok(path)
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
        assert!(!is_url("/home/user/a.zip"));
        assert!(!is_url("a.zip"));
    }

    #[test]
    fn extracts_filename() {
        assert_eq!(url_filename("https://x.com/path/foo.zip"), "foo.zip");
        assert_eq!(url_filename("https://x.com/path/foo.zip?a=b"), "foo.zip");
        assert_eq!(url_filename("https://x.com/foo%20bar.d64"), "foo_20bar.d64");
        assert_eq!(url_filename("https://x.com/"), "download");
    }
}
