use anyhow::Result;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::newsys::utils::copy_dir_all;

#[derive(Default)]
/// Used to pass around files or dirs that can be temporary.
pub struct WorkFile {
    pub path: PathBuf,
    // If Some, must be parent of PathBuf or PathBuf
    pub temp_dir: Option<TempDir>,
    meta: HashMap<String, String>,
}

impl WorkFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            temp_dir: None,
            meta: HashMap::new(),
        }
    }

    pub fn new_with_meta(path: impl Into<PathBuf>, meta: HashMap<String, String>) -> Self {
        Self {
            path: path.into(),
            temp_dir: None,
            meta,
        }
    }

    pub fn new_dir() -> Result<Self> {
        let temp_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
        Ok(Self {
            path: temp_dir.path().into(),
            temp_dir: Some(temp_dir),
            meta: HashMap::new(),
        })
    }

    pub fn new_dir_with_meta(meta: HashMap<String, String>) -> Result<Self> {
        let temp_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
        Ok(Self {
            path: temp_dir.path().into(),
            temp_dir: Some(temp_dir),
            meta,
        })
    }

    pub fn set_meta(&mut self, key: &str, val: impl Into<String>) {
        self.meta.insert(key.into(), val.into());
    }

    pub fn has_meta(&self, key: &str) -> bool {
        self.meta.contains_key(key)
    }

    pub fn get_meta_or(&self, arg: &str, def: impl Into<String>) -> String {
        self.meta.get(arg).map_or(def.into(), |s| s.to_string())
    }

    pub fn get_all_meta(&self) -> HashMap<String, String> {
        self.meta.clone()
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.meta
            .get("tags")
            .map(|t| t.contains(tag))
            .unwrap_or(false)
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    pub fn temp_dir(&self) -> Option<PathBuf> {
        self.temp_dir.as_ref().map(|d| d.path().to_owned())
    }

    // Make sure 'path' is in a temp dir and can be modified
    pub fn make_temp(&mut self) -> Result<()> {
        if self.temp_dir.is_none() {
            let temp_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
            let dir = temp_dir.path();
            if self.path.is_dir() {
                copy_dir_all(&self.path, dir)?;
                self.path = dir.to_path_buf();
            } else {
                let target = dir.join(self.path.file_name().unwrap_or_default());
                fs::copy(&self.path, &target)?;
                self.path = target;
            }
            self.temp_dir = Some(temp_dir);
        }
        Ok(())
    }

    /// Point at another path while keeping the same temp dir alive, e.g. after
    /// unpacking to reach the file inside the directory that was extracted.
    #[must_use]
    pub fn with_path(self, path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..self
        }
    }

    pub fn is_temporary(&self) -> bool {
        self.temp_dir.is_some()
    }

    /// Split into the path and the temp dir backing it. The caller has to hold
    /// on to the [`TempDir`], or the path stops existing.
    pub fn into_parts(self) -> (PathBuf, Option<TempDir>) {
        (self.path, self.temp_dir)
    }
}

impl std::ops::Deref for WorkFile {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for WorkFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<std::ffi::OsStr> for WorkFile {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

impl std::borrow::Borrow<Path> for WorkFile {
    fn borrow(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Debug for WorkFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.path.fmt(f)
    }
}

impl PartialEq for WorkFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for WorkFile {}

impl PartialOrd for WorkFile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for WorkFile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.path.cmp(&other.path)
    }
}

impl std::hash::Hash for WorkFile {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.path.hash(state);
    }
}

impl From<PathBuf> for WorkFile {
    fn from(path: PathBuf) -> Self {
        Self::new(path)
    }
}

impl From<&Path> for WorkFile {
    fn from(path: &Path) -> Self {
        Self::new(path)
    }
}

/// Note that this drops the temp dir, so the path may no longer exist. Use
/// [`WorkFile::into_parts`] to keep it around.
impl From<WorkFile> for PathBuf {
    fn from(work_file: WorkFile) -> Self {
        work_file.path
    }
}
