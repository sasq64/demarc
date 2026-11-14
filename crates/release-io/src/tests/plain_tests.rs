use super::*;
use std::io::Write;

#[test]
fn unpacks_a_zip() {
    let dir = tempfile::tempdir().unwrap();
    let zip_path = dir.path().join("demo.zip");
    let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
    zip.start_file("sub/demo.prg", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&[1, 8, 0]).unwrap();
    zip.finish().unwrap();

    assert!(is_archive(&zip_path).unwrap());
    let out = dir.path().join("out");
    fs::create_dir(&out).unwrap();
    assert!(unpack_into(&zip_path, &out).unwrap());
    assert_eq!(fs::read(out.join("sub/demo.prg")).unwrap(), [1, 8, 0]);
}

#[test]
fn unpacks_gzip_data() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(b"id:1\ttitle:Embryo").unwrap();
    let packed = gz.finish().unwrap();
    assert_eq!(unpack_if_packed(packed).unwrap(), b"id:1\ttitle:Embryo");
    assert_eq!(unpack_if_packed(b"plain".to_vec()).unwrap(), b"plain");
}

#[test]
fn a_rar_is_an_error_and_a_prg_is_not_an_archive() {
    assert!(detect(b"Rar!\x1a\x07\x00").is_err());
    assert_eq!(detect(&[1, 8, 11, 8]).unwrap(), None);
}
