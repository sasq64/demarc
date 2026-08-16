use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;

pub struct M3u {
    pub tags: HashMap<String, String>,
    pub files: Vec<PathBuf>,
}

impl M3u {
    pub fn from_file(path: &Path) -> Result<M3u> {
        let contents = std::fs::read_to_string(path)?;
        let mut tags = HashMap::new();
        let mut files: Vec<PathBuf> = vec![];
        for line in contents.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("#EXTINF:") {
                let mut remaining = rest;
                while let Some(eq) = remaining.find("=\"") {
                    let key_start = remaining[..eq]
                        .rfind(|c: char| c.is_whitespace() || c == ',')
                        .map(|i| i + 1)
                        .unwrap_or(0);
                    let key = remaining[key_start..eq].trim();
                    let after_quote = &remaining[eq + 2..];
                    let Some(end) = after_quote.find('"') else {
                        break;
                    };
                    let value = &after_quote[..end];
                    if !key.is_empty() {
                        tags.insert(key.to_string(), value.to_string());
                    }
                    remaining = &after_quote[end + 1..];
                }
            } else if !line.starts_with('#') {
                files.push(line.into());
            }
        }
        Ok(M3u { tags, files })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Write `text` to `<dir>/<name>` and hand back the path.
    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        path
    }

    fn parse(text: &str) -> (tempfile::TempDir, M3u) {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "list.m3u", text);
        let m3u = M3u::from_file(&path).unwrap();
        (dir, m3u)
    }

    /// Blank lines and comments that aren't `#EXTINF:` are skipped; everything
    /// else is a file entry, taken verbatim.
    #[test]
    fn parses_files_and_skips_comments() {
        let (_dir, m3u) = parse("#EXTM3U\n\ndisk1.d64\n# a comment\n  \nsub/disk2.d64\n");
        assert_eq!(
            m3u.files,
            vec![PathBuf::from("disk1.d64"), PathBuf::from("sub/disk2.d64")]
        );
        assert!(m3u.tags.is_empty(), "unexpected tags: {:?}", m3u.tags);
    }

    /// Several `KEY="value"` pairs on one `#EXTINF:` line, after the leading
    /// duration/title field that the format puts there.
    #[test]
    fn parses_extinf_tags() {
        let (_dir, m3u) = parse("#EXTM3U\n#EXTINF:-1,SYSTEM=\"c64\" DISK=\"1 of 2\"\ndisk1.d64\n");
        assert_eq!(m3u.tags.get("SYSTEM").map(String::as_str), Some("c64"));
        assert_eq!(m3u.tags.get("DISK").map(String::as_str), Some("1 of 2"));
        assert_eq!(m3u.files, vec![PathBuf::from("disk1.d64")]);
    }

    /// An unterminated quote stops tag parsing instead of panicking, and the
    /// tags found before it are kept.
    #[test]
    fn unterminated_quote_stops_tag_parsing() {
        let (_dir, m3u) = parse("#EXTINF:-1,SYSTEM=\"c64\" DISK=\"1\ndisk1.d64\n");
        assert_eq!(m3u.tags.get("SYSTEM").map(String::as_str), Some("c64"));
        assert_eq!(m3u.tags.get("DISK"), None);
        assert_eq!(m3u.files, vec![PathBuf::from("disk1.d64")]);
    }
}
