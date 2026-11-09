//! BoxedApp Packer exes, unpacked into the files they carry and a plain exe.
//!
//! BoxedApp appends a `.bxpck` section holding a virtual file system and a
//! `.main` section with its loader, which hooks file access from a DLL loaded
//! off a virtual path; under wine that loader faults. The original program is
//! left intact apart from the import and TLS directories, which point into
//! `.main`.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tracing::{info, warn};

use crate::newsys::walk_dir;
use crate::workfile::WorkFile;

const PACK_SECTION: &[u8; 8] = b".bxpck\0\0";

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn put_u32(data: &mut [u8], at: usize, value: u32) {
    data[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

struct Section {
    name: [u8; 8],
    va: u32,
    raw: u32,
    raw_size: u32,
    header: usize,
}

struct Pe {
    opt: usize,
    sections: Vec<Section>,
    pack: usize,
}

impl Pe {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.get(..2)? != b"MZ" {
            return None;
        }
        let pe = u32_at(data, 0x3c)? as usize;
        if data.get(pe..pe + 4)? != b"PE\0\0" || u16_at(data, pe + 24)? != 0x10b {
            return None;
        }
        let count = u16_at(data, pe + 6)? as usize;
        let opt = pe + 24;
        let first = opt + u16_at(data, pe + 20)? as usize;
        let mut sections = vec![];
        let mut pack = None;
        for n in 0..count {
            let header = first + n * 40;
            if data.get(header..header + 8)? == PACK_SECTION {
                pack = Some(n);
            }
            sections.push(Section {
                name: data.get(header..header + 8)?.try_into().ok()?,
                va: u32_at(data, header + 12)?,
                raw_size: u32_at(data, header + 16)?,
                raw: u32_at(data, header + 20)?,
                header,
            });
        }
        Some(Pe {
            opt,
            sections,
            pack: pack?,
        })
    }

    /// File offset of `rva` in a section the rebuilt exe keeps.
    fn offset(&self, rva: u32) -> Option<usize> {
        self.sections[..self.pack]
            .iter()
            .find(|s| rva >= s.va && rva - s.va < s.raw_size)
            .map(|s| (rva - s.va + s.raw) as usize)
    }

    fn rva(&self, offset: usize) -> Option<u32> {
        self.sections[..self.pack]
            .iter()
            .find(|s| offset >= s.raw as usize && offset - (s.raw as usize) < s.raw_size as usize)
            .map(|s| (offset - s.raw as usize) as u32 + s.va)
    }
}

/// Files in a `.bxpck` section, as paths relative to the exe's directory and
/// their ranges in `section`. Each entry is a header with name offsets
/// followed by the contents; entries rooted elsewhere (`<SystemRoot>`, `C:`)
/// are skipped.
pub fn files(section: &[u8]) -> Vec<(String, Range<usize>)> {
    let mut found = vec![];
    let mut at = 0;
    while at + 0x4a < section.len() {
        if let Some((size, path, data)) = entry(section, at) {
            if let Some(data) = data
                && !path.starts_with('<')
                && !path.contains(':')
            {
                let path = path.replace('\\', "/");
                if !path.split('/').any(|p| p == ".." || p.is_empty()) {
                    found.push((path, data));
                }
            }
            at += size;
        } else {
            at += 1;
        }
    }
    found
}

fn entry(section: &[u8], at: usize) -> Option<(usize, String, Option<Range<usize>>)> {
    let size = u32_at(section, at)? as usize;
    if u16_at(section, at + 4)? != 5 || u32_at(section, at + 0x36)? != 0x4a {
        return None;
    }
    let path_at = u32_at(section, at + 0x42)? as usize;
    let data_at = u32_at(section, at + 0x46)? as usize;
    let len = u32_at(section, at + 0x2a)? as usize;
    if data_at + len != size || path_at >= data_at || at + size > section.len() {
        return None;
    }
    let name: Vec<u16> = section[at + path_at..at + data_at]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .take_while(|&c| c != 0)
        .collect();
    let path = String::from_utf16(&name).ok()?;
    if path.is_empty() {
        return None;
    }
    let file = (section[at + 0xe] == 0).then_some(at + data_at..at + size);
    Some((size, path, file))
}

/// The longest run of import descriptors in the kept sections: offset, count
/// and the IAT range they cover.
fn find_imports(data: &[u8], pe: &Pe) -> Option<(usize, usize, (u32, u32))> {
    let valid = |at: usize| -> Option<(u32, u32)> {
        let name = pe.offset(u32_at(data, at + 12)?)?;
        let end = name + data.get(name..)?.iter().take(64).position(|&c| c == 0)?;
        let name = data.get(name..end)?;
        if !name.to_ascii_lowercase().ends_with(b".dll")
            || !name.iter().all(|c| c.is_ascii_graphic())
        {
            return None;
        }
        let first = u32_at(data, at + 16)?;
        let mut thunk = pe.offset(first)?;
        while u32_at(data, thunk)? != 0 {
            thunk += 4;
        }
        Some((first, pe.rva(thunk)? + 4))
    };
    let mut best: Option<(usize, usize, (u32, u32))> = None;
    for s in &pe.sections[..pe.pack] {
        let mut at = s.raw as usize;
        let end = (s.raw + s.raw_size) as usize;
        while at + 20 <= end {
            let mut n = 0;
            let mut iat = (u32::MAX, 0);
            while let Some((first, last)) = valid(at + n * 20) {
                iat = (iat.0.min(first), iat.1.max(last));
                n += 1;
            }
            let terminated = data
                .get(at + n * 20..at + n * 20 + 20)
                .is_some_and(|d| d.iter().all(|&b| b == 0));
            if n > 0 && terminated && best.as_ref().is_none_or(|b| n > b.1) {
                best = Some((at, n, iat));
            }
            at += if n > 0 { n * 20 } else { 4 };
        }
    }
    best
}

/// The exe without BoxedApp's sections, with its own imports back.
pub fn rebuild(data: &[u8]) -> Result<Vec<u8>> {
    let Some(pe) = Pe::parse(data) else {
        bail!("Not a BoxedApp exe");
    };
    let pack = &pe.sections[pe.pack];
    if pe.sections[pe.pack + 1..]
        .iter()
        .any(|s| &s.name != b".main\0\0\0")
    {
        bail!("Sections after BoxedApp's");
    }
    let Some((imports, count, iat)) = find_imports(data, &pe) else {
        bail!("No import table outside BoxedApp's sections");
    };
    let mut out = data[..pack.raw as usize].to_vec();
    let dirs = pe.opt + 96;
    for n in 0..u32_at(data, pe.opt + 92).unwrap_or(0).min(16) as usize {
        if u32_at(data, dirs + n * 8).unwrap_or(0) >= pack.va {
            put_u32(&mut out, dirs + n * 8, 0);
            put_u32(&mut out, dirs + n * 8 + 4, 0);
        }
    }
    put_u32(&mut out, dirs + 8, pe.rva(imports).unwrap_or(0));
    put_u32(&mut out, dirs + 12, (count as u32 + 1) * 20);
    put_u32(&mut out, dirs + 12 * 8, iat.0);
    put_u32(&mut out, dirs + 12 * 8 + 4, iat.1 - iat.0);
    let pe_at = pe.opt - 24;
    out[pe_at + 6..pe_at + 8].copy_from_slice(&(pe.pack as u16).to_le_bytes());
    put_u32(&mut out, pe.opt + 56, pack.va);
    put_u32(&mut out, pe.opt + 64, 0);
    for s in &pe.sections[pe.pack..] {
        out[s.header..s.header + 40].fill(0);
    }
    Ok(out)
}

fn packed_exes(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = vec![];
    walk_dir(dir, 2, |path, ext, header| {
        if ext == "exe" && header == b"MZ" {
            let mut head = vec![0; 4096];
            let n = std::io::Read::read(&mut fs::File::open(path)?, &mut head)?;
            if Pe::parse(&head[..n]).is_some() {
                found.push(path.to_owned());
            }
        }
        Ok(())
    })?;
    Ok(found)
}

/// Replace every BoxedApp exe in the release with its [rebuilt](rebuild) form
/// and the files it carries, on a copy.
pub fn unpack_release(file: &mut WorkFile) -> Result<()> {
    if file.is_in_place() || packed_exes(&file.path)?.is_empty() {
        return Ok(());
    }
    file.make_temp()?;
    for exe in packed_exes(&file.path)? {
        if let Err(e) = unpack_exe(&exe) {
            warn!("Leaving BoxedApp exe {exe:?} packed: {e:#}");
        }
    }
    Ok(())
}

fn unpack_exe(exe: &Path) -> Result<()> {
    let data = fs::read(exe)?;
    let rebuilt = rebuild(&data)?;
    let Some(pe) = Pe::parse(&data) else {
        bail!("Not a BoxedApp exe");
    };
    let pack = &pe.sections[pe.pack];
    let section = &data[pack.raw as usize..(pack.raw + pack.raw_size) as usize];
    let dir = exe.parent().unwrap_or(Path::new("."));
    let files = files(section);
    for (path, range) in &files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, &section[range.clone()])?;
    }
    fs::write(exe, rebuilt)?;
    info!("Unpacked BoxedApp exe {exe:?}: {} files", files.len());
    Ok(())
}

#[cfg(test)]
#[path = "tests/boxedapp_tests.rs"]
mod tests;
