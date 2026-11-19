//! The per-release fixups `overrides.toml` carries, as the loading pipeline
//! sees them, and the part of applying one that needs no system.
//!
//! The file itself is parsed by `database::overrides`.

use std::collections::HashMap;
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use qbsdiff::Bspatch;
use retro_core::backend::InputEvent;
use tracing::{debug, info, warn};

use crate::release::walk_dir_find;
use crate::workfile::WorkFile;

/// Meta key: the override's `glsl` patches, which the opengl32 proxy applies to
/// shader sources (`tools/opengl32`). Patches are separated by
/// [`GLSL_PATCH_SEP`], their fields by [`GLSL_FIELD_SEP`]: `h<hash>` or
/// `m<regex>`, then regex/replacement pairs.
pub const META_GLSL_PATCHES: &str = "wine_glsl_patches";
pub const GLSL_PATCH_SEP: char = '\u{1d}';
pub const GLSL_FIELD_SEP: char = '\u{1e}';

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
    pub fn bytes(&self, system_dir: &Path) -> Result<Vec<u8>> {
        if let Some(source) = self.source {
            let path = system_dir.join(source);
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
    // Arguments to start `boot_file` with
    pub boot_args: Vec<&'static str>,
    // Add this meta-data to WorkFile
    pub meta: HashMap<&'static str, &'static str>,
    // Patch these files after unpacking
    pub patches: Vec<Patch>,
    // Run the release on the fast Amiga configuration ([`amiga::apply_fast`](crate::newsys::amiga::apply_fast)),
    // for the ones that need more machine than their year or tags suggest.
    pub fast: bool,
    // Passed to `Backend::send_events` once the backend is created
    pub events: Vec<(u32, InputEvent)>,
    // Replace the download in the cache with the unpacked (and patched)
    // release and run it from there, keeping whatever it writes
    pub cache_writeback: bool,
    // Unpack a self-extracting RAR exe instead of running it
    pub sfx: bool,
}

/// Meta key holding the release directory a `boot_file` was picked out of, set
/// by [`apply_override`] when it narrows the work file's path to one program.
/// A system that copies a release into a drive of its own reads it to know that
/// what it is holding is one file out of a release rather than a loose program.
pub const RELEASE_DIR: &str = "release_dir";

/// Meta key holding the command line of an override's `boot` that has
/// arguments, e.g. `dos4gw.exe demo.exe`.
pub const BOOT_COMMAND: &str = "boot_command";

/// Meta key holding an override's `boot` that names no file in the release,
/// e.g. a file inside a C64 disk image.
pub const BOOT_NAME: &str = "boot_name";

/// Apply what `overrides.toml` says about this release, once it is unpacked and
/// before any system looks at it.
///
/// The parts are independent and any of them may be absent: meta goes on the
/// [`WorkFile`], patches are written into the release, and `boot_file` points
/// the path at the one program to start so that the systems' own file picking
/// is skipped. Patch sources are read from `system_dir`.
pub fn apply_override(file: &mut WorkFile, over: &Override, system_dir: &Path) -> Result<()> {
    apply_override_meta(file, over);
    // An in-place release was patched before it was written back
    if !over.patches.is_empty() && !file.is_in_place() {
        apply_patches(file, &over.patches, system_dir)?;
    }
    if let Some(boot) = over.boot_file {
        if !over.boot_args.is_empty() {
            let command = [&[boot], &over.boot_args[..]].concat().join(" ");
            file.set_meta(BOOT_COMMAND, command);
        }
        match find_named(&release_dir(file), boot)? {
            Some(path) => {
                info!("Override starts {path:?}");
                // A system tells "one loose program the user pointed at" from
                // "a whole release" by whether the work file is a file or a
                // directory, and copies the data files along only in the
                // second case (`copy_all` in `newsys::amiga`). Narrowing the
                // path to the named program throws that away, so leave behind
                // the release it came out of.
                let dir = release_dir(file).to_string_lossy().into_owned();
                file.set_meta(RELEASE_DIR, dir);
                file.path = path;
            }
            // The archive it named is not the one that was downloaded, most
            // likely. Falling back to the systems' own pick still runs
            // something, which beats refusing to load the release at all.
            None => {
                warn!("Override names {boot:?}, which is not in {:?}", file.path);
                file.set_meta(BOOT_NAME, boot);
            }
        }
    }
    Ok(())
}

pub fn apply_override_meta(file: &mut WorkFile, over: &Override) {
    for (key, val) in &over.meta {
        debug!("Override sets {key}={val}");
        file.set_meta(key, *val);
    }
}

/// The directory holding the release, whether the work file points at the
/// directory itself or at one file inside it.
fn release_dir(file: &WorkFile) -> PathBuf {
    if file.path.is_dir() {
        file.path.clone()
    } else {
        file.path.parent().unwrap_or(Path::new(".")).to_owned()
    }
}

/// Patch a release;
/// Will add or modify files in the release directory
/// Must call make_temp() to make sure that is OK.
pub fn apply_patches(file: &mut WorkFile, patches: &[Patch], system_dir: &Path) -> Result<()> {
    file.make_temp()?;
    let dir = release_dir(file);
    for patch in patches {
        let data = patch.bytes(system_dir)?;
        let target = match find_named(&dir, patch.target)? {
            Some(path) => path,
            None => dir.join(patch.target),
        };
        let result = if patch.bsdiff {
            apply_bsdiff(&target, &data)
        } else {
            write_patch(&target, patch.offset, &data)
        };
        result.with_context(|| format!("Could not patch {target:?}"))?;
        info!(
            "Patched {target:?} with {} bytes{}",
            data.len(),
            if patch.info.is_empty() {
                String::new()
            } else {
                format!(" ({})", patch.info)
            }
        );
    }
    Ok(())
}

/// Write `data` into `target`: replacing it entirely when there is no offset,
/// or overwriting the bytes at `offset` when there is. A file too short to
/// reach the offset is extended with zeros.
fn write_patch(target: &Path, offset: Option<usize>, data: &[u8]) -> Result<()> {
    let Some(offset) = offset else {
        return Ok(fs::write(target, data)?);
    };
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(target)?;
    let offset = offset as u64;
    if out.metadata()?.len() < offset {
        out.set_len(offset)?;
    }
    out.seek(SeekFrom::Start(offset))?;
    out.write_all(data)?;
    Ok(())
}

/// Apply a bsdiff patch to `target`, rewriting it with the result. This is how
/// a release whose data file needs a few bytes changed is fixed up without
/// writing the whole file out in the override — see `format = "bsdiff"` in
/// `overrides.toml`.
fn apply_bsdiff(target: &Path, patch: &[u8]) -> Result<()> {
    let source = fs::read(target)?;
    let patcher = Bspatch::new(patch)?;
    let mut out = Vec::with_capacity(patcher.hint_target_size() as usize);
    patcher.apply(&source, &mut out)?;
    fs::write(target, &out)?;
    Ok(())
}

/// A name with a `/` is a path from the release directory, for a name the
/// release has more than once.
fn find_named(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    debug!("Finding {name} in {dir:?}");
    if name.contains('/') {
        let path = dir.join(name);
        return Ok(path.is_file().then_some(path));
    }
    walk_dir_find(dir, 0, |path, _ext, _header| {
        let found = path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.eq_ignore_ascii_case(name));
        Ok(found.then(|| path.to_owned()))
    })
}
