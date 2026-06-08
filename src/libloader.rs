use std::io::Cursor;
use std::path::{Path, PathBuf};

/// File extension of a dynamic library on the current platform.
fn dylib_ext() -> &'static str {
    if cfg!(target_os = "windows") {
        "dll"
    } else if cfg!(target_os = "macos") {
        "dylib"
    } else {
        "so"
    }
}

/// The buildbot path segment naming the current platform, as used in
/// `https://buildbot.libretro.com/nightly/<system>/latest/`.
fn buildbot_system() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows/x86_64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "apple/osx/arm64"
        } else {
            "apple/osx/x86_64"
        }
    } else {
        "linux/x86_64"
    }
}

/// File name of the core's dynamic library, e.g. `snes9x_libretro.so`.
fn dylib_name(name: &str) -> String {
    format!("{name}_libretro.{}", dylib_ext())
}

/// Nightly download URL of the zipped core for the current platform.
fn buildbot_url(name: &str) -> String {
    format!(
        "https://buildbot.libretro.com/nightly/{}/latest/{}.zip",
        buildbot_system(),
        dylib_name(name)
    )
}

/// Download (blocking) the bytes at `url`.
fn download(url: &str) -> anyhow::Result<Vec<u8>> {
    println!("Downloading {url}...");
    let mut reader = ureq::get(url).call()?.into_body().into_reader();
    let mut buf = Vec::new();
    std::io::copy(&mut reader, &mut buf)?;
    Ok(buf)
}

/// Extract the zip archive in `bytes` into `dir`.
fn extract_zip(bytes: &[u8], dir: &Path) -> anyhow::Result<()> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    archive.extract(dir)?;
    Ok(())
}

/// Clear the macOS quarantine attribute so the freshly downloaded library can be
/// dlopen'd without a Gatekeeper prompt. No-op on other platforms.
fn clear_quarantine(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("xattr")
            .args(["-d", "com.apple.quarantine"])
            .arg(path)
            .status();
    }
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}

/// Locate and if necessary download a libretro dynamic library.
/// Check dirs::home_dir() / .lib / libretro / <name> _libretro. <ext>
/// If none existing, download (blocking) from
/// https://buildbot.libretro.com/nightly/<system>/latest/<name>_libretro.<ext>.zip
/// Where system can be "linux/x86_64", "apple/osx/arm64" or "windows/x86_64"
/// and unzip to above mentioned directory.
/// On OSX, make sure quarantine flags are cleared
pub fn get_libretro(name: &str) -> Option<PathBuf> {
    let dir = dirs::home_dir()?.join(".lib").join("libretro");
    get_libretro_in(&dir, name)
}

/// Implementation of [`get_libretro`] against an explicit library directory, so
/// the cache/download logic can be exercised without touching the real home dir.
fn get_libretro_in(dir: &Path, name: &str) -> Option<PathBuf> {
    let target = dir.join(dylib_name(name));
    if target.is_file() {
        return Some(target);
    }

    std::fs::create_dir_all(dir).ok()?;
    let url = buildbot_url(name);
    let bytes = match download(&url) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("Failed to download {url}: {e}");
            return None;
        }
    };
    if let Err(e) = extract_zip(&bytes, dir) {
        tracing::warn!("Failed to extract {url}: {e}");
        return None;
    }

    target.is_file().then(|| {
        clear_quarantine(&target);
        target
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build an in-memory zip containing a single `entry` file with `contents`.
    fn make_zip(entry: &str, contents: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut zw = zip::ZipWriter::new(Cursor::new(&mut buf));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zw.start_file(entry, opts).unwrap();
        zw.write_all(contents).unwrap();
        zw.finish().unwrap();
        buf
    }

    #[test]
    fn url_targets_this_platform() {
        let url = buildbot_url("fceumm");
        assert!(url.starts_with("https://buildbot.libretro.com/nightly/"));
        assert!(url.contains(buildbot_system()));
        assert!(url.ends_with(&format!("fceumm_libretro.{}.zip", dylib_ext())));
    }

    #[test]
    fn returns_cached_library_without_network() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(dylib_name("snes9x"));
        std::fs::write(&path, b"stub").unwrap();

        // File already present, so this must not attempt a download.
        assert_eq!(get_libretro_in(dir.path(), "snes9x"), Some(path));
    }

    #[test]
    fn extracts_library_from_zip() {
        let entry = dylib_name("genesis_plus_gx");
        let zip = make_zip(&entry, b"\x7fELF stub");

        let dir = tempfile::tempdir().unwrap();
        extract_zip(&zip, dir.path()).unwrap();

        let extracted = dir.path().join(&entry);
        assert!(extracted.is_file());
        assert_eq!(std::fs::read(&extracted).unwrap(), b"\x7fELF stub");
    }
}
