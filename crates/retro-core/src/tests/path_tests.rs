use super::*;
use std::path::Path;

#[test]
fn strips_windows_verbatim_prefix() {
    // Cores can't open these; system_dir() must hand out the plain form.
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\C:\demarc\system\amiga")),
        PathBuf::from(r"C:\demarc\system\amiga")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\roms")),
        PathBuf::from(r"\\server\share\roms")
    );
    // Anything without the prefix, and every unix path, is left alone.
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"C:\demarc")),
        PathBuf::from(r"C:\demarc")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new("/home/sasq/system/amiga")),
        PathBuf::from("/home/sasq/system/amiga")
    );
}
