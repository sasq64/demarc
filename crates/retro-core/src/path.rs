//! Path helpers shared with the frontend, kept here because the libretro core
//! needs them and nothing else in `utils` may reach an Android build.

use std::path::{Path, PathBuf};

/// Strip Windows' `\\?\` extended-length path prefix, which `fs::canonicalize`
/// always adds there.
///
/// Nothing but Win32 itself understands those paths. A libretro core reaches
/// the filesystem through the C runtime and its own path joining, and neither
/// copes: amiberry's ROM scan `opendir()`s the directory it is handed, and on a
/// `\\?\` path that call fails outright, so it finds no Kickstart, boots a
/// romless machine and renders a black screen (its path joining also uses `/`,
/// which a verbatim path does *not* accept as a separator — under `\\?\` the
/// string goes to the object manager unparsed). Hand out plain `C:\...` paths.
///
/// A verbatim UNC path (`\\?\UNC\server\share`) becomes `\\server\share`.
/// No-op on paths that don't carry the prefix, and on non-Windows.
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_owned();
    };
    match s.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => PathBuf::from(format!(r"\\{unc}")),
            None => PathBuf::from(rest),
        },
        None => path.to_owned(),
    }
}
#[cfg(test)]
#[path = "tests/path_tests.rs"]
mod tests;
