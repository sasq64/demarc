use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;

use crate::utils::copy_dir_all;

/// Used to pass around files that can be temporary.
pub struct WorkFile {
    pub path: PathBuf,
    // If Some, must be parent of PathBuf or PathBuf
    pub temp_dir: Option<TempDir>,
}

impl WorkFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            temp_dir: None,
        }
    }

    pub fn new_dir() -> Result<Self> {
        let temp_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
        Ok(Self {
            path: temp_dir.path().into(),
            temp_dir: Some(temp_dir),
        })
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    // Make sure 'path' is in a temp dir and can be modified
    pub fn make_temp(&mut self) -> Result<()> {
        if self.temp_dir.is_none() {
            let temp_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
            let dir = temp_dir.path();
            if self.path.is_dir() {
                copy_dir_all(&self.path, &dir)?;
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
