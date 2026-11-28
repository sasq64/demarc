use super::*;
use std::fs;

#[test]
fn needs_the_category_and_prefers_ans() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("readme.nfo"), "x").unwrap();
    fs::write(dir.path().join("art.txt"), "x").unwrap();
    fs::write(dir.path().join("art.ans"), "x").unwrap();
    let sys = AsciiSystem {};
    let mut wf = WorkFile::new(dir.path());
    assert!(!sys.load(&mut wf).unwrap());
    wf.set_meta("category", "ASCII Collection");
    assert!(sys.load(&mut wf).unwrap());
    assert_eq!(wf.path, dir.path().join("art.ans"));
}
