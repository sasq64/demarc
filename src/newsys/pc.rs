//! PC demos: a Windows executable, run under wine (see [`WineEmu`]).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::newsys::walk_dir;
use crate::retro_emu::Backend;
use crate::wine_emu::WineEmu;
use crate::workfile::WorkFile;

use super::System;

/// Enough of a file to find the PE signature its DOS stub points at. The offset
/// lives at 0x3c, so this covers the stub header plus the pointer itself.
const HEADER_LEN: usize = 0x40;

/// Whether `header` is a Windows executable rather than a DOS one.
///
/// `.exe` covers both, and the difference matters: wine runs a PE natively,
/// while a DOS binary needs dosbox and would only fail confusingly here. Every
/// PE keeps a DOS stub, so the `MZ` magic alone says nothing — what separates
/// them is the `PE\0\0` signature the stub's header points to.
fn is_pe(header: &[u8], len: u64) -> bool {
    if header.len() < HEADER_LEN || &header[0..2] != b"MZ" {
        return false;
    }
    let offset = u32::from_le_bytes([header[0x3c], header[0x3d], header[0x3e], header[0x3f]]);
    // Only the offset is in the sniffed header, so the signature it points at
    // has to be read separately — it can be anywhere in the file.
    (offset as u64) + 4 <= len
}

/// Read the four bytes at `offset` and check them against the PE signature.
fn has_pe_signature(path: &Path, header: &[u8]) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let offset = u32::from_le_bytes([header[0x3c], header[0x3d], header[0x3e], header[0x3f]]);
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::Start(offset as u64)).is_err() {
        return false;
    }
    let mut sig = [0u8; 4];
    file.read_exact(&mut sig).is_ok() && &sig == b"PE\0\0"
}

fn is_windows_exe(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(header) = read_header(path) else {
        return false;
    };
    is_pe(&header, meta.len()) && has_pe_signature(path, &header)
}

fn read_header(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut file = fs::File::open(path)?;
    let mut header = vec![0u8; HEADER_LEN];
    let read = file.read(&mut header)?;
    header.truncate(read);
    Ok(header)
}

pub struct PcSystem {}

impl System for PcSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["exe"]
    }

    fn name(&self) -> &'static str {
        "PC"
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path) && is_windows_exe(path)
    }

    /// Pick the executable out of a release directory.
    ///
    /// The trait default takes whichever candidate `read_dir` happens to yield
    /// first, which for a directory holding both `demo.exe` and `setup.exe`
    /// makes the choice depend on the filesystem. Sorting shallowest-first and
    /// then by name at least makes it the same choice every time.
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if !file.is_dir() {
            return Ok(self.can_load(file));
        }
        let mut found: Vec<(usize, PathBuf)> = vec![];
        walk_dir(&file.path.clone(), HEADER_LEN, |path, ext, header| {
            if ext == "exe"
                && is_pe(header, fs::metadata(path)?.len())
                && has_pe_signature(path, header)
            {
                found.push((path.components().count(), path.to_owned()));
            }
            Ok(())
        })?;
        found.sort();
        let Some((_, path)) = found.into_iter().next() else {
            return Ok(false);
        };
        file.path = path;
        Ok(true)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        Ok(Box::new(WineEmu::new(path.as_path(), path.get_all_meta())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A DOS executable has the same `MZ` magic and the same extension, and is
    /// the one thing this must not claim.
    #[test]
    fn dos_stub_alone_is_not_a_windows_exe() {
        // MZ header whose PE offset points past the end of the file.
        let mut header = vec![0u8; HEADER_LEN];
        header[0] = b'M';
        header[1] = b'Z';
        header[0x3c] = 0x80;
        assert!(!is_pe(&header, 0x40));
        // ...and one that points inside it, which only the signature can settle.
        assert!(is_pe(&header, 0x1000));
    }

    /// Write `sig` at the offset the DOS stub points to, and see whether the
    /// file passes as a Windows executable.
    fn exe_with_signature(sig: &[u8; 4]) -> tempfile::NamedTempFile {
        let mut body = vec![0u8; 0x100];
        body[0] = b'M';
        body[1] = b'Z';
        body[0x3c] = 0x80;
        body[0x80..0x84].copy_from_slice(sig);
        let file = tempfile::Builder::new()
            .suffix(".exe")
            .tempfile()
            .expect("temp file");
        fs::write(file.path(), &body).expect("write");
        file
    }

    #[test]
    fn a_pe_signature_is_what_makes_it_loadable() {
        let pe = exe_with_signature(b"PE\0\0");
        assert!(is_windows_exe(pe.path()));
        assert!(PcSystem {}.can_load(pe.path()));

        // Same MZ magic, same extension, no PE — a DOS binary, which needs
        // dosbox rather than wine.
        let dos = exe_with_signature(b"\0\0\0\0");
        assert!(!is_windows_exe(dos.path()));
        assert!(!PcSystem {}.can_load(dos.path()));
    }
}
