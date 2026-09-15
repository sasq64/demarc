use super::*;
use std::path::Path;

/// Convert `testdata/c64/BADALM.T64` (a C64 tape image) to a raw `.prg` file
/// and verify cbmconvert produced it with the expected `$0801` load address.
#[test]
fn t64_to_prg() {
    let t64 = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/c64/BADALM.T64");
    assert!(t64.is_file(), "missing test fixture: {}", t64.display());

    // cbmconvert writes output relative to the CWD, so run inside a temp dir.
    let dir = tempfile::tempdir().unwrap();

    // -t: read T64 input, -N: write native (raw .prg with load address).
    let code = {
        let _guard = CwdGuard::enter(dir.path());
        run(["-t", "-N", t64.to_str().unwrap()])
    };
    assert_eq!(code, 0, "cbmconvert exited with {code}");

    // The T64 entry is named "BADALM"; native output lowercases it.
    let prg = dir.path().join("badalm.prg");
    assert!(prg.is_file(), "expected {} to be created", prg.display());

    let bytes = std::fs::read(&prg).unwrap();
    assert!(bytes.len() > 2, "prg is unexpectedly small");
    // Native .prg starts with a little-endian load address; BADALM is $0801.
    assert_eq!(&bytes[..2], &[0x01, 0x08], "unexpected load address");
}
