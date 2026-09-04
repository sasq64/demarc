use anyhow::{Result, bail};
use std::{
    collections::BTreeMap,
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

pub fn is_disk_image(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        return [
            "d64", "d81", "adf", "dms", "msa", "st", "atr", "xex", "cue", "chd",
        ]
        .contains(&ext.as_str());
    }
    false
}

/// Strip a file comment embedded in an LHA entry's name.
///
/// Amiga LHA archivers store a file's comment in the header's filename field,
/// after a `nul` byte. The reader only honours that convention when the header
/// names Amiga as its OS, which a level 0 header has no room to do, so the
/// comment arrives glued onto the name with the `nul` escaped as `%00`
/// (`dcs-nons.exe%00from _Shape (@b112b.mtalo.ton.tut.fi)`). Cut the name back
/// at the `nul`, escaped or literal, the way `lha` itself does.
fn strip_lha_comment(name: &str) -> &str {
    let cut = [name.find('\0'), name.find("%00")]
        .into_iter()
        .flatten()
        .min();
    match cut {
        Some(i) => &name[..i],
        None => name,
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
        let name = if format == ArchiveFormat::Lha {
            strip_lha_comment(name)
        } else {
            name
        };
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
            let target = dst.as_ref().join(entry.file_name());
            if !target.exists() {
                fs::copy(entry.path(), target)?;
            }
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

/// Strip Windows' `\\?\` extended-length path prefix, which `fs::canonicalize`
/// always adds there.
///
/// Nothing but Win32 itself understands those paths. A libretro core reaches
/// the filesystem through the C runtime and its own path joining, and neither
/// copes: amiberry's ROM scan `opendir()`s the directory it is handed, and on a
/// `\\?\` path that call fails outright, so it finds no Kickstart, boots a
/// romless machine and renders a black screen (its path joining also uses `/`,
/// which a verbatim path does *not* accept as a separator — under `\\?\` the
/// string goes to the object manager unparsed). Hand out plain `C:\...` paths.
///
/// A verbatim UNC path (`\\?\UNC\server\share`) becomes `\\server\share`.
/// No-op on paths that don't carry the prefix, and on non-Windows.
pub fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_owned();
    };
    match s.strip_prefix(r"\\?\") {
        Some(rest) => match rest.strip_prefix(r"UNC\") {
            Some(unc) => PathBuf::from(format!(r"\\{unc}")),
            None => PathBuf::from(rest),
        },
        None => path.to_owned(),
    }
}

#[cfg(test)]
#[path = "tests/utils_tests.rs"]
mod tests;
