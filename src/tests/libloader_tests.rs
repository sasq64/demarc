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
    let fake08 = alt_url("fake08").expect("same platforms as amiberry");
    assert_eq!(core_url("fake08"), fake08);
    assert!(fake08.starts_with("https://github.com/sasq64/fake-08/releases/"));
    assert!(fake08.ends_with(&format!("fake08_libretro-{}.zip", alt_system().unwrap())));
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

/// Fetches the real release, so it is not part of the normal run:
/// `cargo test downloads_fake08 -- --ignored`.
#[test]
#[ignore = "downloads the fake-08 release"]
fn downloads_fake08_from_its_own_release() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = FileCache::at(tmp.path().join("cores"), 200 * 1024 * 1024);
    let path = get_libretro_from(&cache, "fake08").expect("fake08 core");
    assert_eq!(path.file_name().unwrap(), &*dylib_name("fake08"));
    // Smaller than the other cores — a few hundred KB is a real build.
    assert!(std::fs::metadata(&path).unwrap().len() > 256 * 1024);
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
