use std::{
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use anyhow::{Result, bail};

use super::{entry_path, unpack_7z};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    Zip,
    Gz,
    SevenZ,
}

/// The format `header` starts with. An archive this build cannot open is an
/// error rather than "not an archive", so it is not mistaken for a program.
fn detect(header: &[u8]) -> Result<Option<Format>> {
    if header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06") {
        Ok(Some(Format::Zip))
    } else if header.starts_with(&[0x1f, 0x8b]) {
        Ok(Some(Format::Gz))
    } else if header.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Ok(Some(Format::SevenZ))
    } else if header.starts_with(b"Rar!") {
        bail!("RAR archives are not supported on this platform")
    } else if header.len() > 6 && &header[2..5] == b"-lh" && header[6] == b'-' {
        bail!("LHA archives are not supported on this platform")
    } else {
        Ok(None)
    }
}

fn detect_file(path: &Path) -> Result<Option<Format>> {
    let mut header = Vec::new();
    fs::File::open(path)?.take(8).read_to_end(&mut header)?;
    detect(&header)
}

pub fn unpack_if_packed(data: Vec<u8>) -> Result<Vec<u8>> {
    if detect(&data)? != Some(Format::Gz) {
        return Ok(data);
    }
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(&data[..]).read_to_end(&mut out)?;
    Ok(out)
}

pub fn has_archive_filename(path: &Path) -> Result<bool> {
    Ok(super::has_any_extension(path, &["zip", "gz", "7z"]))
}

pub fn is_archive(path: &Path) -> Result<bool> {
    Ok(detect_file(path)?.is_some())
}

pub fn unpack_sfx(_path: &Path, _target_dir: &Path) -> Result<bool> {
    bail!("Self-extracting RAR archives are not supported on this platform")
}

pub fn unpack_into(path: &Path, target_dir: &Path) -> Result<bool> {
    let Some(format) = detect_file(path)? else {
        return Ok(false);
    };
    let mut file = BufReader::new(fs::File::open(path)?);
    file.seek(SeekFrom::Start(0))?;
    match format {
        Format::SevenZ => unpack_7z(file, target_dir)?,
        Format::Gz => {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("data");
            if let Some(out) = entry_path(target_dir, stem) {
                let mut gz = flate2::read::MultiGzDecoder::new(file);
                std::io::copy(&mut gz, &mut fs::File::create(out)?)?;
            }
        }
        Format::Zip => {
            let mut archive = zip::ZipArchive::new(file)?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i)?;
                let Some(out) = entry_path(target_dir, entry.name()) else {
                    continue;
                };
                if entry.is_dir() {
                    fs::create_dir_all(&out)?;
                    continue;
                }
                if let Some(parent) = out.parent() {
                    fs::create_dir_all(parent)?;
                }
                std::io::copy(&mut entry, &mut fs::File::create(&out)?)?;
            }
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "../tests/plain_tests.rs"]
mod tests;
