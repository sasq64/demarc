use anyhow::Result;
use std::{
    fs,
    io::{BufReader, Write},
    path::{Path, PathBuf},
};
use unarc_rs::unified::ArchiveFormat;

pub fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

pub fn has_any_extension(path: &Path, ext: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ext.contains(&e))
}

fn is_same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn build_m3u(files: &[impl AsRef<Path>], target_dir: &Path) -> Result<PathBuf> {
    let mut contents = String::from("#EXTM3U\n");
    for file in files {
        let file: &Path = file.as_ref();
        let name = file
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
        let target = target_dir.join(name);
        if !is_same_file(file, &target) {
            fs::copy(file, &target)?;
        }
        contents.push_str(&name.to_string_lossy());
        contents.push('\n');
    }

    let m3u_path = target_dir.join("demo.m3u");
    let mut m3u = fs::File::create(&m3u_path)?;
    m3u.write_all(contents.as_bytes())?;
    m3u.flush()?;
    Ok(m3u_path)
}

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
    use std::{io::BufReader, path::Component};

    let mut file = BufReader::new(fs::File::open(path)?);
    let Some(format) = ArchiveFormat::detect(&mut file, Some(path))? else {
        return Ok(false);
    };
    if !is_supported_archive(format) {
        return Ok(false);
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
        // Keep only normal path components so an absolute path or `..` in the
        // archive can't write outside the target directory.
        let rel: PathBuf = Path::new(name)
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .collect();
        if rel.as_os_str().is_empty() {
            // Unusable name (e.g. all `..`): nothing safe to write.
            archive.skip(&entry)?;
            continue;
        }
        let out_path = target_dir.join(&rel);
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
/// Read exactly `len` bytes from the start of `path`. Fails with
/// [`std::io::ErrorKind::UnexpectedEof`] if the file is shorter.
pub fn read_header(path: &Path, len: usize) -> std::io::Result<Vec<u8>> {
    let got = read_at(path, 0, len)?;
    if got.len() < len {
        Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            format!(
                "{}: expected {} byte header, file is only {} bytes",
                path.display(),
                len,
                got.len()
            ),
        ))
    } else {
        Ok(got)
    }
}

/// Read up to `len` bytes of `path` starting at `offset`. Returns fewer bytes
/// if the file ends first.
pub fn read_at(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut buf = vec![0u8; len];
    let mut file = fs::File::open(path)?;
    if offset != 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
    let mut got = 0;
    while got < len {
        match file.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    buf.truncate(got);
    Ok(buf)
}

pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

pub fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name));
        matches.then_some(path)
    })
}
