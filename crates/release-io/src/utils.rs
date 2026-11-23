use anyhow::Result;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub use retro_core::strip_verbatim_prefix;

mod unarc;

#[cfg(test)]
use unarc::strip_lha_comment;
pub use unarc::{has_archive_filename, is_archive, unpack_if_packed, unpack_into, unpack_sfx};

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

pub fn get_ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
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

pub fn is_disk_image(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        return [
            "d64", "d81", "adf", "dms", "msa", "st", "stx", "dim", "atr", "xex", "cue", "chd",
        ]
        .contains(&ext.as_str());
    }
    false
}

/// Where an archive entry called `name` goes under `target_dir`, keeping only
/// normal path components so an absolute path or `..` can't escape it. `None`
/// when nothing is left of the name. DOS-made zips separate with `\`.
fn entry_path(target_dir: &Path, name: &str) -> Option<PathBuf> {
    use std::path::Component;
    let name = name.replace('\\', "/");
    let rel: PathBuf = Path::new(&name)
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .collect();
    (!rel.as_os_str().is_empty()).then(|| target_dir.join(rel))
}

/// Extract a 7z in one pass over each block. unarc-rs reads entries one at a
/// time, which in a solid archive decodes the block from its start for every
/// file.
fn unpack_7z(file: impl std::io::Read + std::io::Seek, target_dir: &Path) -> Result<()> {
    let mut archive = sevenz_rust2::ArchiveReader::new(file, sevenz_rust2::Password::empty())?;
    archive.for_each_entries(|entry, reader| {
        let Some(out_path) = entry_path(target_dir, entry.name()) else {
            std::io::copy(reader, &mut std::io::sink())?;
            return Ok(true);
        };
        if entry.is_directory() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            std::io::copy(reader, &mut fs::File::create(&out_path)?)?;
        }
        Ok(true)
    })?;
    Ok(())
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

/// Copy `src` into `dst`, keeping files already there. Names are matched
/// ignoring case, as the Amiga and Atari drives these build see them, so `C/`
/// merges into an existing `c/` instead of shadowing it.
pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    let dst = dst.as_ref();
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let existing = entry.file_name().to_str().and_then(|n| find_child(dst, n));
        let target = existing.unwrap_or_else(|| dst.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            copy_dir_all(entry.path(), target)?;
        } else if !target.exists() {
            fs::copy(entry.path(), target)?;
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

// Sort disk images. Procedure:
// - Iterate and decide the number of each disk.
//  The digit just before the dot if no digit before it OR
//  The uppercase letter before the dot if not upper case before it OR
//  The last separate digit in the file name unless at the start
//
//  example
//
//  disk3.adf -> 3
//  45degreesA.adf -> 1
//  3witches.dsk -> None
//  game_B.DMS -> 2
//  GOA.dsk -> None
//
// After this step, if there are gaps in the list (normally the 1st slot)
// select one of the non sorted disk for the empty slot, preferring the one
// who starts with the same letters as the first sorted disk
//
//  space.adf -> None
//  Space_disk2.adf -> 2
//  extra.adf -> None
//
//  Selects "space.adf" as slot 1
//
// If multiple disks end up in the same slot then the preference is the same,
// use digits over letters
//
//  disk_A.adf
//  disk_1.adf
//  disk_B.adf
//  disk_2.adf
//
//  -> [ disk_1.adf, disk_2.adf]
//
// If every disk ends up in the same slot the numbering is meaningless, so
// sort the names normally instead
//
//  intro3.adf
//  credits3.adf
//
//  -> [ credits3.adf, intro3.adf ]
//
pub fn sort_disks(paths: &mut [PathBuf]) {
    if paths.len() < 2 {
        return;
    }

    let stems: Vec<String> = paths
        .iter()
        .map(|p| {
            p.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        })
        .collect();

    // Walk the names in a fixed order so that claims the rules can't separate
    // (`disk_1.adf` vs `disk_2.adf` both wanting slot 1 by their letter) always
    // resolve the same way, whatever order the directory listing arrived in.
    let mut order: Vec<usize> = (0..paths.len()).collect();
    order.sort_by_key(|&i| stems[i].to_lowercase());

    let claims: Vec<Option<DiskSlot>> = stems.iter().map(|s| disk_slot(s)).collect();

    // Every name landing in the same slot (`intro3`, `credits3`) means those
    // numbers aren't disk numbers at all, so they say nothing about the order.
    // Sort by name instead of letting one arbitrary disk win the slot and the
    // rest be shuffled by the digit-beats-letter and gap filling rules.
    let first_claim = claims[0].map(DiskSlot::number);
    if first_claim.is_some()
        && claims
            .iter()
            .all(|c| c.map(DiskSlot::number) == first_claim)
    {
        let sorted: Vec<PathBuf> = order.iter().map(|&i| paths[i].clone()).collect();
        paths.clone_from_slice(&sorted);
        return;
    }

    // slot number -> (index into `paths`, claimed by a digit)
    let mut slots: BTreeMap<u32, (usize, bool)> = BTreeMap::new();
    let mut leftover: Vec<usize> = Vec::new();
    for &i in &order {
        let Some(claim) = claims[i] else {
            leftover.push(i);
            continue;
        };
        match slots.get_mut(&claim.number()) {
            // A digit is a stronger claim than a letter; the loser drops back
            // into the unsorted pile.
            Some(held) if claim.is_digit() && !held.1 => {
                leftover.push(std::mem::replace(held, (i, true)).0);
            }
            Some(_) => leftover.push(i),
            None => {
                slots.insert(claim.number(), (i, claim.is_digit()));
            }
        }
    }

    // Fill any hole below the highest numbered disk (usually slot 1, left empty
    // because the first disk of a set is often named without a number) from the
    // unsorted pile, preferring a name that starts like the first sorted disk.
    if let Some((&first_slot, &(first, _))) = slots.iter().next() {
        let start = if first_slot == 0 { 0 } else { 1 };
        let last = *slots.keys().next_back().expect("slots is not empty");
        let gaps: Vec<u32> = (start..last).filter(|s| !slots.contains_key(s)).collect();
        for gap in gaps {
            let mut best: Option<(usize, usize)> = None; // (position in leftover, prefix len)
            for (pos, &i) in leftover.iter().enumerate() {
                let shared = common_prefix_len(&stems[i], &stems[first]);
                if best.is_none_or(|(_, top)| shared > top) {
                    best = Some((pos, shared));
                }
            }
            let Some((pos, _)) = best else { break };
            slots.insert(gap, (leftover.remove(pos), false));
        }
    }

    let sorted: Vec<PathBuf> = slots
        .values()
        .map(|&(i, _)| i)
        .chain(leftover)
        .map(|i| paths[i].clone())
        .collect();
    paths.clone_from_slice(&sorted);
}

/// A disk name's claim on a slot in the set. Two names can claim the same slot,
/// in which case the digit wins — see [`sort_disks`].
#[derive(Clone, Copy)]
enum DiskSlot {
    Digit(u32),
    Letter(u32),
}

impl DiskSlot {
    fn number(self) -> u32 {
        match self {
            Self::Digit(n) | Self::Letter(n) => n,
        }
    }

    fn is_digit(self) -> bool {
        matches!(self, Self::Digit(_))
    }
}

/// Which disk of a set `stem` (a file name without its extension) names, if any.
fn disk_slot(stem: &str) -> Option<DiskSlot> {
    let chars: Vec<char> = stem.chars().collect();
    let last = *chars.last()?;
    let prev = chars.len().checked_sub(2).map(|i| chars[i]);

    // `disk3` -- a trailing digit, as long as it's a digit on its own and not
    // the tail of a longer number (`shadow1992`).
    if last.is_ascii_digit() && !prev.is_some_and(|c| c.is_ascii_digit()) {
        return last.to_digit(10).map(DiskSlot::Digit);
    }
    // `game_B` -- a trailing capital, as long as it isn't just the end of a
    // word already in capitals (`GOA`).
    if last.is_ascii_uppercase() && !prev.is_some_and(|c| c.is_ascii_uppercase()) {
        return Some(DiskSlot::Letter(last as u32 - 'A' as u32 + 1));
    }
    // Otherwise the last standalone digit anywhere in the name (`disk2_[cr]`),
    // but never one that opens it -- `3witches` is a title, not a disk number.
    let mut found = None;
    for (i, &c) in chars.iter().enumerate() {
        let standalone = i > 0
            && c.is_ascii_digit()
            && !chars[i - 1].is_ascii_digit()
            && !chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
        if standalone && let Some(d) = c.to_digit(10) {
            found = Some(DiskSlot::Digit(d));
        }
    }
    found
}

/// How many leading characters `a` and `b` share, ignoring case.
fn common_prefix_len(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(a, b)| a.eq_ignore_ascii_case(b))
        .count()
}

#[cfg(test)]
#[path = "tests/utils_tests.rs"]
mod tests;
