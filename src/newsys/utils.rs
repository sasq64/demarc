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

pub fn sort_disks(files: &mut [PathBuf]) {
    fn rank(path: &Path) -> u8 {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if stem.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            0
        } else if stem.chars().any(|c| c.is_ascii_digit()) {
            1
        } else {
            2
        }
    }

    files.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
}

pub fn get_disk_images(dir: &Path, exts: &[&str]) -> Result<Vec<PathBuf>> {
    let mut disk_images = vec![];

    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        println!("{path:?}");

        if path.is_dir() {
            let sub = get_disk_images(&path, exts)?;
            disk_images.extend(sub);
            continue;
        };
        if has_any_extension(&path, exts) {
            disk_images.push(path);
        }
    }
    sort_disks(&mut disk_images);
    Ok(disk_images)
}

pub fn build_m3u(files: &[impl AsRef<Path>], target_dir: &Path) -> Result<PathBuf> {
    let mut contents = String::from("#EXTM3U\n");
    for file in files {
        let file: &Path = file.as_ref();
        let name = file
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
        fs::copy(file, target_dir.join(name))?;
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
