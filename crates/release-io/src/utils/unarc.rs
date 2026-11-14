use std::{fs, io::BufReader, path::Path};

use anyhow::{Result, bail};
use unarc_rs::unified::ArchiveFormat;

use super::{entry_path, unpack_7z};

/// Archive formats [`unpack_to_temp`] knows how to extract.
fn is_supported_archive(format: ArchiveFormat) -> bool {
    matches!(
        format,
        ArchiveFormat::Zip
            | ArchiveFormat::SevenZ
            | ArchiveFormat::Rar
            | ArchiveFormat::Lha
            | ArchiveFormat::Tar
    ) || is_single_file_compressor(format)
}

/// Formats that compress one unnamed payload rather than holding a set of named
/// files, so the payload's name has to come from the archive's own (see
/// [`unpack_into`]) and the whole thing can be unpacked straight to bytes (see
/// [`unpack_if_packed`]).
fn is_single_file_compressor(format: ArchiveFormat) -> bool {
    matches!(
        format,
        ArchiveFormat::Z | ArchiveFormat::Gz | ArchiveFormat::Bz2
    )
}

/// Decompress `data` when it is a gzip, bzip2 or Unix-compress stream, and
/// return it unchanged otherwise. For data files that are packed on their own
/// instead of bundled in an archive — a gzipped db — where the point is the
/// bytes, not files on disk as with [`unpack_into`].
pub fn unpack_if_packed(data: Vec<u8>) -> Result<Vec<u8>> {
    let Some(format) = ArchiveFormat::detect_from_bytes(&data) else {
        return Ok(data);
    };
    if !is_single_file_compressor(format) {
        return Ok(data);
    }
    let mut archive = format.open(std::io::Cursor::new(&data[..]))?;
    let Some(entry) = archive.next_entry()? else {
        bail!("{} stream holds nothing", format.name());
    };
    Ok(archive.read(&entry)?)
}

/// Strip a file comment embedded in an LHA entry's name.
///
/// Amiga LHA archivers store a file's comment in the header's filename field,
/// after a `nul` byte. The reader only honours that convention when the header
/// names Amiga as its OS, which a level 0 header has no room to do, so the
/// comment arrives glued onto the name with the `nul` escaped as `%00`
/// (`dcs-nons.exe%00from _Shape (@b112b.mtalo.ton.tut.fi)`). Cut the name back
/// at the `nul`, escaped or literal, the way `lha` itself does.
pub(super) fn strip_lha_comment(name: &str) -> &str {
    let cut = [name.find('\0'), name.find("%00")]
        .into_iter()
        .flatten()
        .min();
    match cut {
        Some(i) => &name[..i],
        None => name,
    }
}

pub fn has_archive_filename(path: &Path) -> Result<bool> {
    if let Some(fmt) = ArchiveFormat::from_path(path) {
        Ok(is_supported_archive(fmt))
    } else {
        Ok(false)
    }
}

pub fn is_archive(path: &Path) -> Result<bool> {
    let mut file = BufReader::new(fs::File::open(path)?);
    let Some(format) = ArchiveFormat::detect(&mut file, Some(path))? else {
        return Ok(false);
    };
    // Tar is often reported falsely by file detection
    if format == ArchiveFormat::Tar && ArchiveFormat::from_path(path) != Some(ArchiveFormat::Tar) {
        return Ok(false);
    }
    if !is_supported_archive(format) {
        return Ok(false);
    }
    Ok(true)
}

/// Extract the archive at `path` into the existing directory `target_dir`,
/// which is written into directly — see [`unpack_to_temp`] for the variant that
/// makes a temp directory of its own. Returns `false`, having written nothing,
/// when `path` is not a recognised archive.
pub fn unpack_into(path: &Path, target_dir: &Path) -> Result<bool> {
    unpack_as(path, target_dir, None)
}

/// [`unpack_into`] for a self-extracting RAR exe.
pub fn unpack_sfx(path: &Path, target_dir: &Path) -> Result<bool> {
    unpack_as(path, target_dir, Some(ArchiveFormat::Rar))
}

fn unpack_as(path: &Path, target_dir: &Path, format: Option<ArchiveFormat>) -> Result<bool> {
    let mut file = BufReader::new(fs::File::open(path)?);
    let format = match format {
        Some(format) => format,
        None => match ArchiveFormat::detect(&mut file, Some(path))? {
            Some(format) => format,
            None => return Ok(false),
        },
    };
    if !is_supported_archive(format) {
        return Ok(false);
    }

    if format == ArchiveFormat::SevenZ {
        unpack_7z(file, target_dir)?;
        return Ok(true);
    }

    let mut archive = format.open(file)?;
    // Single-file compressors (.Z/.gz/.bz2) carry no name for their payload, so
    // derive one from the archive's stem (e.g. `demo.tar.gz` -> `demo.tar`).
    if is_single_file_compressor(format)
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        archive.set_single_file_name(stem.to_string());
    }

    while let Some(entry) = archive.next_entry()? {
        let name = entry.name();
        let name = if format == ArchiveFormat::Lha {
            strip_lha_comment(name)
        } else {
            name
        };
        let Some(out_path) = entry_path(target_dir, name) else {
            archive.skip(&entry)?;
            continue;
        };
        if name.ends_with('/') || name.ends_with('\\') {
            // Explicit directory entry (zip, tar).
            fs::create_dir_all(&out_path)?;
            archive.skip(&entry)?;
            continue;
        }
        // Some formats (e.g. rar) mark directories only in per-file metadata the
        // unified reader doesn't expose, but always decompress them to nothing.
        // Treating an empty entry as a directory both handles those and keeps a
        // zero-length file from blocking a later `dir/child` from being created.
        let data = archive.read(&entry)?;
        if data.is_empty() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out_path, &data)?;
        }
    }
    Ok(true)
}
