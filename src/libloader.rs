use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use crate::cache::FileCache;

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

/// Cores the libretro buildbot does not ship, paired with the base url of the
/// release that does. Each holds one zip per platform, named
/// `<name>_libretro-<system>.zip` and containing the library under the same
/// name the buildbot uses, so nothing downstream has to know the difference.
const ALT_SOURCES: &[(&str, &str)] = &[(
    "amiberry",
    "https://github.com/sasq64/amiberry/releases/download/latest",
)];

/// The platform segment used by the [`ALT_SOURCES`] archives, which name
/// platforms their own way rather than the buildbot's. `None` on a platform
/// no such release builds for.
fn alt_system() -> Option<&'static str> {
    if cfg!(target_os = "windows") {
        Some("windows-x64")
    } else if cfg!(target_os = "macos") {
        cfg!(target_arch = "aarch64").then_some("macos-arm64")
    } else if cfg!(target_arch = "x86_64") {
        Some("linux-x86_64")
    } else {
        None
    }
}

/// Download url of `name` from its own release, if it has one for this
/// platform.
fn alt_url(name: &str) -> Option<String> {
    let (_, base) = ALT_SOURCES.iter().find(|(core, _)| *core == name)?;
    Some(format!("{base}/{name}_libretro-{}.zip", alt_system()?))
}

/// Where the zipped core for the current platform is downloaded from: the
/// core's own release when [`ALT_SOURCES`] names one, the libretro nightly
/// buildbot otherwise.
fn core_url(name: &str) -> String {
    alt_url(name).unwrap_or_else(|| buildbot_url(name))
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

/// Downloaded cores, one entry per buildbot url, holding the unzipped library.
///
/// Keyed on the url rather than on the core name, so the platform is part of
/// the key and a cache copied between machines can't hand back a library for
/// the wrong one.
static CORES: LazyLock<FileCache> =
    LazyLock::new(|| FileCache::new("cores", CACHE_LIMIT).with_max_age(MAX_AGE));

/// A dozen or so cores at a few tens of MB each, with room for the ones a
/// user tried once and stopped opening — those are what eviction is for.
const CACHE_LIMIT: u64 = 500 * 1024 * 1024;

/// How long a downloaded core is used before it is fetched again.
///
/// The buildbot url names `latest`, so an entry keyed on it goes out of date
/// on its own schedule rather than when anything demarc knows about changes.
/// A fortnight keeps up with fixes upstream without making a launch depend on
/// the network — and when the network isn't there,
/// [`FileCache::with_max_age`]'s fallback keeps the copy already downloaded.
const MAX_AGE: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Trim the core cache back under its budget ([`CACHE_LIMIT`], or whatever the
/// cache's `.limit` says). Intended to run once at startup, before anything
/// asks for a core, so this run's own cores can't be evicted out from under it.
pub fn prune_cache() {
    CORES.prune();
}

/// Locate and if necessary download a libretro dynamic library.
///
/// A hand-built core in `$DEMARC_CORE_DIR` or `<system dir>/cores` wins over
/// the buildbot (see [`local_core`]); otherwise the library is
/// cached under the user's cache directory, refetched from
/// `https://buildbot.libretro.com/nightly/<system>/latest/<name>_libretro.<ext>.zip`
/// when the copy there is older than [`MAX_AGE`] — where `<system>` is
/// "linux/x86_64", "apple/osx/arm64" or "windows/x86_64". Cores the buildbot
/// does not ship come from their own release instead, see [`ALT_SOURCES`].
///
/// Returns `None` only if there is nothing usable at all: a download that
/// fails with a cached core to fall back on still returns that core. On macOS
/// the quarantine flag is cleared so the result can be `dlopen`ed.
pub fn get_libretro(name: &str) -> Option<PathBuf> {
    if let Some(path) = local_core(name) {
        return Some(path);
    }
    get_libretro_from(&CORES, name)
}

/// A locally built core, taking precedence over the buildbot.
///
/// `$DEMARC_CORE_DIR` is a colon-separated list of directories searched for
/// `<name>_libretro.<ext>`; the first hit wins. Nothing else in the cache path
/// runs, so a local build is never written to (or evicted from) the cache.
/// Exists so a core built from source can be tested without overwriting the
/// downloaded copy.
fn local_core(name: &str) -> Option<PathBuf> {
    let path = local_core_in(&std::env::var_os("DEMARC_CORE_DIR")?, name)?;
    tracing::info!("Using local core {}", path.display());
    Some(path)
}

/// Implementation of [`local_core`] against an explicit path list, so the
/// search can be exercised without setting an environment variable.
fn local_core_in(dirs: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    let lib = dylib_name(name);
    std::env::split_paths(dirs).find_map(|dir| {
        let path = dir.join(&lib);
        path.is_file().then_some(path)
    })
}

/// Implementation of [`get_libretro`] against an explicit cache, so the
/// download logic can be exercised without touching the user's real one.
fn get_libretro_from(cache: &FileCache, name: &str) -> Option<PathBuf> {
    let url = core_url(name);
    let lib = dylib_name(name);
    // The library itself is the marker: an archive that unpacked to anything
    // else is not a core, and must not be cached as one.
    let entry = cache.get_dir(&url, &lib, |dir| {
        let bytes = download(&url)?;
        extract_zip(&bytes, dir)?;
        anyhow::ensure!(dir.join(&lib).is_file(), "{url} contains no {lib}");
        Ok(())
    });
    match entry {
        Ok(dir) => {
            let path = dir.join(&lib);
            clear_quarantine(&path);
            Some(path)
        }
        Err(e) => {
            tracing::warn!("Failed to get libretro core {name}: {e}");
            None
        }
    }
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
    fn alt_source_url_names_this_platform() {
        let Some(url) = alt_url("amiberry") else {
            return; // No release for this platform; the buildbot url stands.
        };
        assert_eq!(core_url("amiberry"), url);
        assert!(url.starts_with("https://github.com/sasq64/amiberry/releases/"));
        assert!(url.ends_with(&format!("amiberry_libretro-{}.zip", alt_system().unwrap())));
        // A core with no alternative source still goes to the buildbot.
        assert_eq!(alt_url("snes9x"), None);
        assert_eq!(core_url("snes9x"), buildbot_url("snes9x"));
    }

    #[test]
    fn returns_cached_library_without_network() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::at(tmp.path().join("cores"), 1024 * 1024);
        // Populate the entry the way a successful download would, so the
        // lookup under test is a hit and never reaches the buildbot.
        let entry = cache
            .get_dir(&core_url("snes9x"), &dylib_name("snes9x"), |dir| {
                std::fs::write(dir.join(dylib_name("snes9x")), b"stub")?;
                Ok(())
            })
            .unwrap();

        assert_eq!(
            get_libretro_from(&cache, "snes9x"),
            Some(entry.join(dylib_name("snes9x")))
        );
    }

    #[test]
    fn local_core_found_in_search_path() {
        let empty = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let lib = dylib_name("amiberry");
        std::fs::write(dir.path().join(&lib), b"local build").unwrap();

        let dirs = std::env::join_paths([empty.path(), dir.path()]).unwrap();
        assert_eq!(
            local_core_in(&dirs, "amiberry"),
            Some(dir.path().join(&lib))
        );
        // A core no directory holds falls through to the download path.
        assert_eq!(local_core_in(&dirs, "snes9x"), None);
    }

    /// Fetches the real release, so it is not part of the normal run:
    /// `cargo test downloads_amiberry -- --ignored`.
    #[test]
    #[ignore = "downloads the amiberry release"]
    fn downloads_amiberry_from_its_own_release() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = FileCache::at(tmp.path().join("cores"), 200 * 1024 * 1024);
        let path = get_libretro_from(&cache, "amiberry").expect("amiberry core");
        assert_eq!(path.file_name().unwrap(), &*dylib_name("amiberry"));
        assert!(std::fs::metadata(&path).unwrap().len() > 1024 * 1024);
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
