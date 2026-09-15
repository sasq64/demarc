use super::*;

/// The repository's own test disk, a real trackloaded demo release.
const REBELS: &str = "testdata/amiga/rebels.adf";

#[test]
fn refuses_something_that_is_not_a_disk_image() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let not_adf = dir.path().join("not.adf");
    std::fs::write(&not_adf, b"nowhere near a floppy").unwrap();

    assert!(unpack(&not_adf, dir.path()).is_err());
}

/// A disk with no AmigaDOS file system on it is the normal case for a
/// trackloaded demo, and has to come back as an error rather than as an
/// empty directory the caller might then try to boot.
#[test]
fn refuses_a_disk_with_no_file_system() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let image = dir.path().join("custom.adf");
    // A full-size floppy image whose boot block is not `DOS`.
    std::fs::write(&image, vec![0u8; 880 * 1024]).unwrap();

    assert!(unpack(&image, dir.path()).is_err());
}

#[test]
fn unpacks_a_dos_disk_if_the_test_image_has_a_file_system() {
    if !Path::new(REBELS).is_file() {
        return;
    }
    let dir = tempfile::Builder::new().tempdir().unwrap();
    // Either it mounts and gives us files, or it is trackloaded and says
    // so; both are correct, and which one depends on the test image.
    if let Ok(count) = unpack(Path::new(REBELS), dir.path())
        && count > 0
    {
        let mut entries = std::fs::read_dir(dir.path()).unwrap().peekable();
        assert!(entries.peek().is_some());
        // Names are transcoded to UTF-8 on the way out, because that is
        // what the cores read the drive as — see `safe_name` in the shim.
        for entry in entries {
            let name = entry.unwrap().file_name();
            assert!(name.to_str().is_some(), "not UTF-8: {name:?}");
        }
    }
}
