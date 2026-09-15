use super::*;

/// A real DMS release: Anarchy's 3D Demo II, an AmigaDOS disk.
const ARCHIVE: &str = "testdata/ANARCHY-3dDemo2.dms";

/// The size of an unpacked double-density Amiga floppy: 80 cylinders of two
/// 5632-byte tracks.
const ADF_SIZE: u64 = 901_120;

fn test_archive() -> Option<PathBuf> {
    let path = PathBuf::from(ARCHIVE);
    path.is_file().then_some(path)
}

#[test]
fn unpacks_a_real_archive_into_a_bootable_disk() {
    let Some(archive) = test_archive() else {
        return;
    };
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let image = dir.path().join("disk.adf");

    assert_eq!(unpack(&archive, &image).unwrap(), ADF_SIZE);
    assert_eq!(std::fs::metadata(&image).unwrap().len(), ADF_SIZE);
    // The AmigaDOS boot block, which is what makes this worth unpacking: a
    // trackloaded disk would come out with something else here.
    assert_eq!(&std::fs::read(&image).unwrap()[..3], b"DOS");
}

/// The unpacked image is the point of the exercise: ADFlib has to be able to
/// walk it, or `--unadf` gains nothing from any of this.
#[test]
fn the_unpacked_disk_is_one_adflib_can_read() {
    let Some(archive) = test_archive() else {
        return;
    };
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let image = dir.path().join("disk.adf");
    let dest = dir.path().join("out");
    std::fs::create_dir(&dest).unwrap();

    unpack(&archive, &image).unwrap();
    assert!(super::super::adf::unpack(&image, &dest).unwrap() > 0);
    // A disk demarc would boot as a hard drive has to have one of these.
    assert!(dest.join("s").join("startup-sequence").is_file());
}

#[test]
fn refuses_something_that_is_not_a_dms_archive() {
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let not_dms = dir.path().join("not.dms");
    std::fs::write(&not_dms, b"nowhere near a floppy").unwrap();

    assert!(unpack(&not_dms, &dir.path().join("disk.adf")).is_err());
}

/// A valid header with no tracks behind it is the shape a truncated download
/// takes. It has to come back as an error rather than as an empty image the
/// caller then tries to mount.
#[test]
fn refuses_an_archive_with_no_tracks_in_it() {
    let Some(archive) = test_archive() else {
        return;
    };
    let dir = tempfile::Builder::new().tempdir().unwrap();
    let truncated = dir.path().join("truncated.dms");
    // The real header, so it passes the "DMS!" check and its own CRC.
    std::fs::write(&truncated, &std::fs::read(&archive).unwrap()[..56]).unwrap();

    assert!(unpack(&truncated, &dir.path().join("disk.adf")).is_err());
}

#[test]
fn knows_a_dms_by_its_name() {
    assert!(is_dms(Path::new("ANARCHY-3dDemo2.dms")));
    assert!(is_dms(Path::new("SHOUTING.DMS")));
    assert!(!is_dms(Path::new("rebels.adf")));
}
