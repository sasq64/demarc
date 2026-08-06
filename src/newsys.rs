use crate::retro_emu::{Backend, RetroCoreThreaded};
use crate::system_dir;
use crate::utils::{copy_dir_all, is_archive, scan_release_dir, unpack_into};
use crate::{cbmconvert, libloader};
use anyhow::{Result, bail};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use tracing::{info, warn};

pub enum CanLoad {
    Yes,
    No,
    Maybe,
}

/// Used to pass around files that can be temporary.
pub struct WorkFile {
    path: PathBuf,
    // If Some, must be parent of PathBuf or PathBuf
    temp_dir: Option<TempDir>,
}

impl WorkFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            temp_dir: None,
        }
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

fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

fn build_m3u(files: &[impl AsRef<Path>], target_dir: &Path) -> Result<PathBuf> {
    let mut contents = String::from("#EXTM3U\n");
    for file in files {
        let file: &Path = file.as_ref();
        let name = file
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
        fs::copy(file, target_dir.join(name))?;
        contents.push_str(&name.to_string_lossy());
        contents.push('\n');
    }

    let m3u_path = target_dir.join("demo.m3u");
    let mut m3u = fs::File::create(&m3u_path)?;
    m3u.write_all(contents.as_bytes())?;
    m3u.flush()?;
    Ok(m3u_path)
}

pub trait System {
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn core_name(&self) -> &'static str {
        ""
    }

    fn can_load(&self, ext: &str) -> CanLoad {
        if self.extensions().contains(&ext) {
            CanLoad::Yes
        } else {
            CanLoad::No
        }
    }

    // Try to load a program with this system.
    fn load(&self, _file: &mut WorkFile, _tags: &HashMap<String, String>) -> Result<bool> {
        Ok(false)
    }

    fn create(
        &self,
        path: &WorkFile,
        tags: &HashMap<String, String>,
    ) -> Result<Box<dyn Backend + Send + Sync>> {
        println!("PATH {path:?}");
        let core = libloader::get_libretro(self.core_name()).unwrap();
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            tags.clone(),
            false,
        )?))
    }
}

const CORE_NAME_VICE_64SC: &str = "vice_x64sc";
// const CORE_NAME_VICE_64: &str = "vice_x64";
// const CORE_NAME_VICE_DTV: &str = "vice_x64dtv";
// const CORE_NAME_VICE_128: &str = "vice_x128";
// const CORE_NAME_VICE_C16: &str = "vice_xplus4";
// const CORE_NAME_VICE_VIC20: &str = "vice_xvic";

struct C64System {}

impl C64System {
    fn convert_files(path: &Path) -> Result<()> {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                let path = entry?.path();
                Self::convert_files(&path)?;
            }
        } else if has_extension(&path, "t64") {
            info!("Converting {path:?}");
            let _guard = cbmconvert::CwdGuard::enter(path.parent().unwrap());
            let code = cbmconvert::run(["-t", "-N", path.to_string_lossy().as_ref()]);
            if code != 0 {
                warn!("cbmconvert failed on {path:?} (exit code {code})");
            }
        }
        Ok(())
    }
}

impl System for C64System {
    fn extensions(&self) -> &'static [&'static str] {
        &["d64", "prg", "d81", "t64"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_VICE_64SC
    }
    fn load(&self, file: &mut WorkFile, _tags: &HashMap<String, String>) -> Result<bool> {
        if file.is_dir() || has_extension(file, "t64") {
            file.make_temp()?;
            Self::convert_files(file);
            let scanned = scan_release_dir(file)?;
            if !scanned.disk_images.is_empty() {
                let m3u = build_m3u(&scanned.disk_images, file)?;
                file.path = m3u;
            }
        }
        Ok(true)
    }
}

fn get_systems() -> Vec<Box<dyn System>> {
    vec![Box::new(C64System {})]
}

pub fn load_file(path: &Path) -> Result<Box<dyn System>> {
    let mut wf = WorkFile::new(path);
    if is_archive(path)? {
        wf = WorkFile::new();
        unpack_into(path, &wf);
    }

    for sys in get_systems() {
        if sys.load(&mut wf, &HashMap::new()).unwrap() {
            let mut backend = sys.create(&wf, &HashMap::new()).unwrap();
            backend.skip_frames(100);
            backend.run();
        }
    }
    bail!("NO")
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_prg() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let demos = root.join("testdata").join("c64");
        let mut wf = WorkFile::new(demos.join("quantum.prg"));

        let ext = wf.extension().unwrap().to_str().unwrap().to_owned();

        for sys in get_systems() {
            if sys.load(&mut wf, &HashMap::new()).unwrap() {
                let mut backend = sys.create(&wf, &HashMap::new()).unwrap();
                backend.skip_frames(100);
                backend.run();
            }
        }

        // let sys = get_systems()
        //     .into_iter()
        //     .find(|sys| matches!(sys.can_load(&ext), CanLoad::Yes))
        //     .expect("no system for .prg");
        //
        // if let Some(wf) = sys.load(wf, &HashMap::new()).unwrap() {
        //     assert!(wf.exists());
        //     let mut backend = sys.create(&wf, &HashMap::new()).unwrap();
        //     backend.run();
        // }
    }
}
