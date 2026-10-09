//! The per-release fixups `overrides.toml` carries, as the systems see them.
//!
//! The file itself is parsed by demarc (`src/overrides.rs`); these are only the
//! two types the loading pipeline passes down, which is why they live here and
//! not there.

use std::collections::HashMap;

use anyhow::{Context, Result};
use retro_core::backend::InputEvent;

use crate::system_dir;

/// Give a runtime-built string the `'static` lifetime an an override wants.
///
/// The file list is built once and kept for the whole run, so nothing collected
/// into it is ever freed anyway; leaking says so in the type and lets entries
/// hold plain `&'static str` instead of `String`. Only used for the handful of
/// strings that aren't already slices of the leaked db text — m3u tags and file
/// stems, and the overrides read at startup — so the
/// leak is bounded by the size of the file list.
pub fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

#[derive(Default, Debug, Clone)]
pub struct Patch {
    // File name of file to be patched
    pub target: &'static str,
    // Offset into file where data goes. None means replace entire file (normal case)
    pub offset: Option<usize>,
    // Data, base64 encoded
    pub data: &'static str,
    // If Some, data is read from this file in the system dir instead
    pub source: Option<&'static str>,
    // If true, data is a bsdiff patch to apply to the target, not the new contents
    pub bsdiff: bool,
    // Info to user
    pub info: &'static str,
}

impl Patch {
    /// The bytes to write, read from [`Self::source`] or decoded from [`Self::data`].
    pub fn bytes(&self) -> Result<Vec<u8>> {
        if let Some(source) = self.source {
            let path = system_dir().join(source);
            return std::fs::read(&path).with_context(|| format!("Could not read {path:?}"));
        }
        use base64::Engine;
        // A delta is thousands of characters, so it is written wrapped over as
        // many lines in the toml; the decoder wants none of that whitespace.
        let data: String = self.data.split_whitespace().collect();
        base64::engine::general_purpose::STANDARD
            .decode(&data)
            .with_context(|| format!("Bad base64 in patch for {:?}", self.target))
    }
}

/// A per-release fixup, read from `overrides.toml` and keyed on the demozoo id
/// of the release it is for — see `overrides.toml`.
///
/// A release the db describes correctly needs none of this; these are for the
/// ones where the db's own answer is wrong or ambiguous — several downloads
/// where only one is the demo, an archive holding more than one program, a DOS
/// release whose sound config has to say GUS before it makes any noise.
#[derive(Default, Debug, Clone)]
pub struct Override {
    // If Some, select the URL ending with this file-name for download
    pub download: Option<&'static str>,
    // If Some, download this URL instead of anything the db lists
    pub download_url: Option<&'static str>,
    // If Some, override file selection by system and pass this file directly to load()
    pub boot_file: Option<&'static str>,
    // Add this meta-data to WorkFile
    pub meta: HashMap<&'static str, &'static str>,
    // Patch these files after unpacking
    pub patches: Vec<Patch>,
    // Run the release on the fast Amiga configuration ([`amiga::apply_fast`](crate::newsys::amiga::apply_fast)),
    // for the ones that need more machine than their year or tags suggest.
    pub fast: bool,
    // Passed to `Backend::send_events` once the backend is created
    pub events: Vec<(u32, InputEvent)>,
}
