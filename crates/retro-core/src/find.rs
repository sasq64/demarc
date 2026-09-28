//! Locating a libretro core on disk, without any of demarc's download
//! machinery. On Android this is where the "load in place from the APK's
//! native library directory" arm goes — see `docs/ANDROID.md`.

use std::path::PathBuf;
use std::sync::OnceLock;

pub fn dylib_name(name: &str) -> String {
    let ext = if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    };
    format!("{name}_libretro.{ext}")
}

/// On Android the core ships inside the APK, in the native library directory
/// the dynamic linker already searches, so the "path" is its bare file name.
/// Nothing may be loaded from the writable data dir there.
#[cfg(target_os = "android")]
pub fn find_core(name: &str) -> Option<PathBuf> {
    Some(PathBuf::from(format!("lib{}", dylib_name(name))))
}

/// Find a libretro core without any of demarc's download machinery: the
/// `DEMARC_CORE_DIR` list, the directory holding this executable, then the
/// per-url subdirectories of demarc's own core cache.
#[cfg(not(target_os = "android"))]
pub fn find_core(name: &str) -> Option<PathBuf> {
    let file = dylib_name(name);
    let mut dirs: Vec<PathBuf> = std::env::var_os("DEMARC_CORE_DIR")
        .iter()
        .flat_map(|list| std::env::split_paths(list).collect::<Vec<_>>())
        .collect();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        dirs.push(dir.to_owned());
    }
    let cache = dirs::cache_dir().unwrap_or_default().join("demarc/cores");
    dirs.extend(
        std::fs::read_dir(cache)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path()),
    );
    dirs.into_iter()
        .map(|dir| dir.join(&file))
        .find(|path| path.is_file())
}

static SYSTEM_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Set the directory [`system_dir`] returns. `android_main` uses it to hand the
/// core the app's private data directory, which is the only writable place
/// there and is not something the environment can be asked for.
pub fn set_system_dir(dir: PathBuf) {
    _ = SYSTEM_DIR.set(dir);
}

/// Directory handed to the core as its libretro system and save directory.
pub fn system_dir() -> PathBuf {
    if let Some(dir) = SYSTEM_DIR.get() {
        return dir.clone();
    }
    if let Some(dir) = std::env::var_os("DEMARC_SYSTEM_DIR") {
        return PathBuf::from(dir);
    }
    let local = PathBuf::from("system");
    if local.is_dir() {
        return local;
    }
    let dir = dirs::cache_dir().unwrap_or_default().join("demarc/system");
    _ = std::fs::create_dir_all(&dir);
    dir
}
