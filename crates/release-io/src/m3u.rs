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

#[allow(dead_code)]
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

        let mut m3u = fs::File::create(target)?;
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

        let mut m3u = fs::File::create(target)?;
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
#[path = "tests/m3u_tests.rs"]
mod tests;
