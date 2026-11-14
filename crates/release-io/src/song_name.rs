//! Music file names, split at their type.

// Only allow these extensions to avoid crashes
pub const EXTENSIONS: &[&str] = &[
    "sid", // C64
    "mod", "xm", "s3m", "ft", "stm", "it", // Trackers
    "snd", "sndh", "sap", // Atari
    "nsf", "gbs", "spc", "psf", // Console
    "mp3", "flac", // Streaming
    "v2m",  //
    "emul", "vtx", "pt1", "pt2", "pt3", "asc", "sqt", "stc", "stp", "psc", // Spectrum
    "smod", "dm2", "ahx", "aon", "mt2", "mon", "dw", "fred", "smod", "hip", "cus", "fc", "hvl",
    "cm", "fp", "syn", "ma", "hipc", "ml", "mk2", "bd", "dln", "669", // Amiga
    "jam", "dbm", "bp", "bp3", "hes", "lds", "mdat", "tfx", "tfmx",
];

/// Types that can come first in a file name, Amiga style: `mdat.song`.
const NAME_PREFIXES: &[&str] = &["mod", "mdat", "xm", "stk"];

/// Types that only play with a second file beside them, and that file's type.
/// It is named like the song, with its type on the same end.
const SECONDARY_FILES: &[(&str, &str)] = &[("mdat", "smpl")];

/// A music file name, split at its type.
pub struct SongName<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub prefixed: bool,
}

impl<'a> SongName<'a> {
    /// A known extension wins over a prefix, since `mod.acker.nsf` is an NSF.
    pub fn parse(file: &'a str) -> Option<Self> {
        let (title, kind) = file.rsplit_once('.')?;
        let (prefix, rest) = file.split_once('.')?;
        let known = |types: &[&str], name: &str| types.iter().any(|t| t.eq_ignore_ascii_case(name));
        if known(NAME_PREFIXES, prefix) && !known(EXTENSIONS, kind) {
            return Some(Self {
                kind: prefix,
                title: rest,
                prefixed: true,
            });
        }
        Some(Self {
            kind,
            title,
            prefixed: false,
        })
    }

    /// The name of the file this song needs beside it, if it needs one.
    pub fn secondary(&self) -> Option<String> {
        let (_, other) = SECONDARY_FILES
            .iter()
            .find(|(kind, _)| self.kind.eq_ignore_ascii_case(kind))?;
        Some(if self.prefixed {
            format!("{other}.{}", self.title)
        } else {
            format!("{}.{other}", self.title)
        })
    }
}
