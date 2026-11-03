//! RIFE frame interpolation, from `librife_demarc` (`just rife`) loaded at run
//! time so that nothing is built or linked unless it is there.

use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

mod worker;
pub use worker::{Frame, Status, Worker};

type New = unsafe extern "C" fn(c_int, *const c_char) -> *mut c_void;
type Process =
    unsafe extern "C" fn(*mut c_void, *const u32, *const u32, c_int, c_int, f32, *mut u32) -> c_int;
type Free = unsafe extern "C" fn(*mut c_void);

pub const LIB_NAME: &str = if cfg!(target_os = "windows") {
    "rife_demarc.dll"
} else if cfg!(target_os = "macos") {
    "librife_demarc.dylib"
} else {
    "librife_demarc.so"
};

/// Where the library is looked for: `$DEMARC_RIFE`, beside the executable, and
/// in `target/rife` for a build from the repo.
pub fn lib_candidates() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::env::var_os("DEMARC_RIFE")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_owned))
    {
        paths.push(dir.join(LIB_NAME));
        paths.push(dir.join("../rife").join(LIB_NAME));
    }
    paths
}

pub struct Rife {
    handle: *mut c_void,
    process: Process,
    free: Free,
    _lib: libloading::Library,
}

// The handle is only used through `&mut self`.
unsafe impl Send for Rife {}

impl Rife {
    /// Load the first library of [`lib_candidates`] that exists, with the RIFE v4
    /// model in `model_dir`, on the default GPU.
    pub fn load(model_dir: &Path) -> anyhow::Result<Self> {
        let lib = lib_candidates()
            .into_iter()
            .find(|path| path.is_file())
            .with_context(|| format!("{LIB_NAME} not found (build it with `just rife`)"))?;
        Self::load_from(&lib, model_dir)
    }

    pub fn load_from(lib: &Path, model_dir: &Path) -> anyhow::Result<Self> {
        let dir = CString::new(model_dir.to_string_lossy().as_bytes())?;
        unsafe {
            let lib = libloading::Library::new(lib)
                .with_context(|| format!("loading {}", lib.display()))?;
            let new: New = *lib.get(b"rife_new\0")?;
            let process: Process = *lib.get(b"rife_process\0")?;
            let free: Free = *lib.get(b"rife_free\0")?;
            let handle = new(-1, dir.as_ptr());
            if handle.is_null() {
                bail!(
                    "RIFE could not start on a GPU with the model in {}",
                    model_dir.display()
                );
            }
            Ok(Self {
                handle,
                process,
                free,
                _lib: lib,
            })
        }
    }

    /// The frame at `t` of the way from `a` to `b`, both `width * height` packed
    /// RGBA pixels, into `out`.
    pub fn interpolate(
        &mut self,
        a: &[u32],
        b: &[u32],
        width: u32,
        height: u32,
        t: f32,
        out: &mut Vec<u32>,
    ) -> anyhow::Result<()> {
        let n = (width * height) as usize;
        if a.len() < n || b.len() < n {
            bail!("frames smaller than {width}x{height}");
        }
        out.resize(n, 0);
        let err = unsafe {
            (self.process)(
                self.handle,
                a.as_ptr(),
                b.as_ptr(),
                width as c_int,
                height as c_int,
                t,
                out.as_mut_ptr(),
            )
        };
        if err != 0 {
            bail!("RIFE failed ({err})");
        }
        Ok(())
    }
}

impl Drop for Rife {
    fn drop(&mut self) {
        unsafe { (self.free)(self.handle) }
    }
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
