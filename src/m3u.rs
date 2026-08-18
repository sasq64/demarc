use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

#[derive(Debug, Default)]
pub struct M3u {
    pub tags: HashMap<String, String>,
    pub files: Vec<PathBuf>,
}

fn is_same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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

    pub fn build(files: &[impl AsRef<Path>]) -> Result<M3u> {
        let mut m3u = M3u::default();
        for file in files {
            m3u.files.push(file.as_ref().to_owned());
        }
        Ok(m3u)
    }

    pub fn write_to(&self, target: &Path) -> Result<()> {
        let mut contents = String::from("#EXTM3U\n");
        if !self.tags.is_empty() {
            contents.push_str("#EXTINF:");
            for (key, value) in &self.tags {
                contents.push_str(&format!(" {key}=\"{value}\""));
            }
            contents.push('\n');
        }
        for file in &self.files {
            contents.push_str(&file.to_string_lossy());
            contents.push('\n');
        }

        let mut m3u = fs::File::create(&target)?;
        m3u.write_all(contents.as_bytes())?;
        m3u.flush()?;
        Ok(())
    }

    /// Write this m3u, and make sure all files are copied into the same directory
    /// 'target' must have a parent directory
    pub fn relocate(&self, target: &Path) -> Result<()> {
        let target_dir = target
            .parent()
            .context("Target must have parent directory")?;
        let mut contents = String::from("#EXTM3U\n");
        for file in &self.files {
            let file: &Path = file.as_ref();
            let name = file
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
            let target = target_dir.join(name);
            if !is_same_file(file, &target) {
                fs::copy(file, &target)?;
            }
            contents.push_str(&name.to_string_lossy());
            contents.push('\n');
        }

        let mut m3u = fs::File::create(&target)?;
        m3u.write_all(contents.as_bytes())?;
        m3u.flush()?;
        Ok(())
    }

    /// Verify that all M3U files exist (in the given parent, unless absolute)
    pub fn verify(&self, parent: &Path) -> Result<bool> {
        for file in &self.files {
            let file: &Path = file.as_ref();
            let target = parent.join(file);
            if !target.exists() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// `write_to` emits a header plus one line per file, and the file list
    /// survives a write/read round trip.
    #[test]
    fn write_to_round_trips_files() {
        let dir = tempfile::tempdir().unwrap();
        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![PathBuf::from("a.d64"), PathBuf::from("b.d64")],
        };
        let out = dir.path().join("out.m3u");
        m3u.write_to(&out).unwrap();

        assert_eq!(fs::read_to_string(&out).unwrap(), "#EXTM3U\na.d64\nb.d64\n");
        assert_eq!(M3u::from_file(&out).unwrap().files, m3u.files);
    }

    /// Tags are written back as quoted `KEY="value"` pairs on one `#EXTINF:`
    /// line, so `from_file` reads them again unchanged.
    #[test]
    fn write_to_round_trips_tags() {
        let dir = tempfile::tempdir().unwrap();
        let tags: HashMap<String, String> = [
            ("SYSTEM".to_string(), "c64".to_string()),
            ("DISK".to_string(), "1 of 2".to_string()),
        ]
        .into_iter()
        .collect();
        let m3u = M3u {
            tags: tags.clone(),
            files: vec![PathBuf::from("a.d64")],
        };
        let out = dir.path().join("out.m3u");
        m3u.write_to(&out).unwrap();

        // Tag order follows HashMap iteration, so compare the parsed map.
        let read = M3u::from_file(&out).unwrap();
        assert_eq!(read.tags, tags);
        assert_eq!(read.files, m3u.files);
    }

    /// The exact output for a single tag, since round-tripping alone would
    /// pass even if both sides agreed on a broken format.
    #[test]
    fn write_to_quotes_tag_values() {
        let dir = tempfile::tempdir().unwrap();
        let m3u = M3u {
            tags: [("SYSTEM".to_string(), "c64".to_string())]
                .into_iter()
                .collect(),
            files: vec![PathBuf::from("a.d64")],
        };
        let out = dir.path().join("out.m3u");
        m3u.write_to(&out).unwrap();

        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "#EXTM3U\n#EXTINF: SYSTEM=\"c64\"\na.d64\n"
        );
    }

    /// `relocate` copies every entry next to the target and rewrites the list
    /// to plain file names.
    #[test]
    fn relocate_copies_files_next_to_target() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let a = write(src.path(), "a.d64", "aaa");
        let b = write(src.path(), "b.d64", "bbb");

        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![a, b],
        };
        let out = dst.path().join("out.m3u");
        m3u.relocate(&out).unwrap();

        assert_eq!(fs::read_to_string(&out).unwrap(), "#EXTM3U\na.d64\nb.d64\n");
        assert_eq!(fs::read_to_string(dst.path().join("a.d64")).unwrap(), "aaa");
        assert_eq!(fs::read_to_string(dst.path().join("b.d64")).unwrap(), "bbb");
        assert!(m3u.verify(dst.path()).unwrap());
    }

    /// Relocating in place must not copy a file onto itself (which would
    /// truncate it); `is_same_file` is what guards against that.
    #[test]
    fn relocate_in_place_keeps_contents() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.d64", "aaa");

        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![a.clone()],
        };
        m3u.relocate(&dir.path().join("out.m3u")).unwrap();

        assert_eq!(fs::read_to_string(&a).unwrap(), "aaa");
    }

    /// `verify` is false as soon as one entry is missing from the parent dir.
    #[test]
    fn verify_detects_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.d64", "aaa");

        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![PathBuf::from("a.d64"), PathBuf::from("b.d64")],
        };
        assert!(!m3u.verify(dir.path()).unwrap());

        write(dir.path(), "b.d64", "bbb");
        assert!(m3u.verify(dir.path()).unwrap());
    }

    /// Relative entries are resolved against `parent` as written, sub-directory
    /// and all — a same-named file sitting directly in `parent` doesn't count.
    #[test]
    fn verify_resolves_relative_subdirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        write(dir.path(), "disk.d64", "wrong place");

        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![PathBuf::from("sub/disk.d64")],
        };
        assert!(!m3u.verify(dir.path()).unwrap());

        write(&dir.path().join("sub"), "disk.d64", "right place");
        assert!(m3u.verify(dir.path()).unwrap());
    }

    /// Absolute entries are checked where they actually point, ignoring
    /// `parent` entirely.
    #[test]
    fn verify_uses_absolute_paths_as_is() {
        let elsewhere = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        let a = write(elsewhere.path(), "a.d64", "aaa");
        assert!(a.is_absolute(), "tempdir should give an absolute path");

        let m3u = M3u {
            tags: HashMap::new(),
            files: vec![a.clone()],
        };
        assert!(m3u.verify(parent.path()).unwrap());

        fs::remove_file(&a).unwrap();
        assert!(!m3u.verify(parent.path()).unwrap());
    }
}
