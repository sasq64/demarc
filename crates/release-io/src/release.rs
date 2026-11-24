//! Getting a downloaded release unpacked and its files found, before any
//! system looks at it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, trace};

use crate::m3u::M3u;
use crate::utils::{
    has_archive_filename, has_extension, is_archive, read_at, sort_disks, unpack_into, unpack_sfx,
};
use crate::workfile::WorkFile;

/// Is a meta value one of the ways of saying yes?
pub fn is_yes(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "on"
    )
}

/// Walk `dir`, calling `call` with the path, lowercased extension and the
/// first `header_size` bytes of every file big enough to have them.
pub fn walk_dir(
    dir: &Path,
    header_size: usize,
    mut call: impl FnMut(&Path, &str, &[u8]) -> Result<()>,
) -> Result<()> {
    walk_dir_find(dir, header_size, |path, ext, header| {
        trace!("WALK: {path:?}");
        call(path, ext, header)?;
        Ok(None::<()>)
    })?;
    Ok(())
}

/// [`walk_dir`] for searches: the first `Some` the callback returns ends the
/// walk and comes back as the result.
pub fn walk_dir_find<T>(
    dir: &Path,
    header_size: usize,
    mut call: impl FnMut(&Path, &str, &[u8]) -> Result<Option<T>>,
) -> Result<Option<T>> {
    for f in walkdir::WalkDir::new(dir).into_iter() {
        let file = f?;
        if file
            .file_name()
            .to_str()
            .unwrap_or_default()
            .starts_with(".")
        {
            continue;
        }
        if file.path().is_file() {
            let header = read_at(file.path(), 0, header_size)?;
            if header.len() == header_size {
                let ext = file
                    .path()
                    .extension()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if let Some(res) = call(file.path(), &ext, &header)? {
                    return Ok(Some(res));
                }
            }
        }
    }
    Ok(None)
}

pub fn collect_disk_images(file: &mut WorkFile, images: &mut [PathBuf]) -> Result<()> {
    if !images.is_empty() {
        if images.len() > 1 {
            info!("IMAGES {images:?}");
            sort_disks(images);
            info!("IMAGES {images:?}");
            // images can be in a non-writeable or temp dir
            let m3u = M3u::build(images)?;
            file.make_temp()?;
            let demo_m3u = file
                .temp_dir()
                .context("Should now be in tempdir")?
                .join("demo.m3u");
            info!("m3u: {demo_m3u:?}");
            m3u.relocate(&demo_m3u)?;
            file.path = demo_m3u;
        } else {
            file.path = images[0].clone();
        }
    }
    Ok(())
}

/// Unpack a downloaded release, producing the [`WorkFile`] that
/// [`NewSys::load_prepared`] takes over from.
///
/// Split out from the rest because it touches no shared state at all: it reads
/// `path` and writes into a temp dir of its own, so the frontend runs it on the
/// I/O pool while the release currently on screen keeps playing (see
/// `loading::load_async`). [`NewSys::load_prepared`] then runs on the I/O pool
/// too, but only once this half is in: what it does to the release — and to the
/// core it replaces — cannot start until the download has landed.
///
/// Archives are unpacked one level deep and then once more, because scene
/// releases are routinely packed inside another archive. An m3u is not
/// unpacked at all; its tags become meta and the directory it names is what
/// gets loaded.
pub fn unpack_release(path: &Path, meta: &HashMap<String, String>) -> Result<WorkFile> {
    debug!("Trying to load: {path:?}");
    let mut wf = WorkFile::new_with_meta(path, meta.clone());
    if path.is_dir() && meta.get(DISK_SET).is_some_and(|v| is_yes(v)) {
        wf = WorkFile::new_dir_with_meta(meta.clone())?;
        for entry in std::fs::read_dir(path)? {
            let file = entry?.path();
            if is_archive(&file)? {
                unpack_into(&file, &wf)?;
            } else if let Some(name) = file.file_name() {
                std::fs::copy(&file, wf.join(name))?;
            }
        }
    } else if path.is_file() {
        if meta.get(SFX).is_some_and(|v| is_yes(v)) {
            wf = WorkFile::new_dir_with_meta(meta.clone())?;
            debug!("Unpacking self-extracting {path:?} to {wf:?}");
            unpack_sfx(path, &wf)?;
        } else if is_archive(path)? {
            wf = WorkFile::new_dir_with_meta(meta.clone())?;
            if let Some(stem) = path.file_stem() {
                wf.set_meta(ARCHIVE_NAME, stem.to_string_lossy());
            }
            debug!("Unpacking {path:?} to {wf:?}");
            unpack_into(path, &wf)?;
            let has_exe = walk_dir_find(&wf, 2, |_, _, header| {
                Ok(matches!(header, b"MZ" | b"ZM").then_some(()))
            })?
            .is_some();
            if !has_exe {
                walk_dir(&wf, 4, |f, _, _| {
                    if has_archive_filename(f)? {
                        debug!("File was double packed");
                        unpack_into(f, &wf)?;
                    }
                    Ok(())
                })?;
            }
        } else if has_extension(path, "m3u") {
            // TODO: We should not collect m3us
            let m3u = M3u::from_file(path)?;
            wf.path = path.parent().unwrap_or(path).to_owned();
            for (key, value) in m3u.tags {
                wf.set_meta(&key, value);
            }
        }
    }
    Ok(wf)
}

/// Meta key set by an override's `sfx`: the release is a self-extracting RAR.
pub const SFX: &str = "sfx";

/// Meta key set by an override's `disk_set`: the downloads are the disks of a
/// set, each one possibly in an archive of its own.
pub const DISK_SET: &str = "disk_set";

/// Meta key holding the name of the archive a release was unpacked from, which
/// the temp dir it now sits in no longer carries.
pub const ARCHIVE_NAME: &str = "archive_name";
