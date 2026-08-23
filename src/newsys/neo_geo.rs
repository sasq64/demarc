use anyhow::Result;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tracing::{debug, info, warn};

use super::System;
use super::disc::{
    DiscImage, IsoSpec, TRACK_EXTENSIONS, build_iso, cue_data_tracks, cue_is_complete, iso_name,
};
use crate::{
    cache::{FileCache, KeyHasher},
    newsys::walk_dir,
    workfile::WorkFile,
};

const CORE_NAME_GEOLITH: &str = "geolith";

/// The boot list at the root of every Neo Geo CD disc: one
/// `NAME.EXT,<bank>,<offset>` line per file, which the BIOS reads and loads into
/// the memory area the extension picks (`.PRG` to work RAM, `.FIX` to the fix
/// layer, `.SPR` to sprite RAM, `.Z80` to the sound CPU, `.PCM` to ADPCM). Its
/// presence in the root is what makes a disc — or a directory — a Neo Geo CD one.
const IPL_NAME: &str = "IPL.TXT";

/// Volume descriptor fields a pressed Neo Geo CD fills in, and the files they
/// name. Nothing in the boot path reads them, but a real disc has them and they
/// cost three directory entries that are on the disc anyway.
const COPYRIGHT_FILE: &str = "CPY.TXT";
const ABSTRACT_FILE: &str = "ABS.TXT";
const BIBLIO_FILE: &str = "BIB.TXT";

/// Whether one of `files` is a [`IPL_NAME`] — which is to say, whether the
/// directory they came from is a Neo Geo CD release rather than a directory of
/// unrelated ones. [`crate::files::collect_files`] asks before it splits a
/// directory into one playlist entry per file, since these files are a single
/// disc and only mean anything together.
pub fn holds_boot_list(files: &[PathBuf]) -> bool {
    files.iter().any(|path| {
        path.file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(IPL_NAME))
    })
}

/// Whether the disc image at `path` is a Neo Geo CD one, judged by [`IPL_NAME`]
/// in its root directory. Works on a bare `.iso` and on the raw data track a
/// cue names alike — see [`DiscImage`], which sorts the sector layout out.
fn is_neogeo_cd_disc(path: &Path) -> bool {
    DiscImage::open(path).is_some_and(|mut disc| disc.root_names().iter().any(|n| n == IPL_NAME))
}

/// Whether the cue sheet at `path` describes a Neo Geo CD, judged by the data
/// track it names. A sheet of nothing but audio tracks is a plain CD, and one
/// whose data track belongs to another console isn't ours. One that can't find
/// all of its files is no use to the core at all, so it isn't taken either.
fn is_neogeo_cd_cue(path: &Path) -> bool {
    cue_is_complete(path)
        && cue_data_tracks(path)
            .iter()
            .any(|track| is_neogeo_cd_disc(track))
}

/// The file names [`IPL_NAME`] asks the BIOS to load, upper cased. Only used to
/// report a disc that would boot into a hang because a file it names is missing.
fn ipl_entries(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.split(',').next())
        .map(|name| name.trim().trim_end_matches('\u{1a}').to_ascii_uppercase())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Burn the directory holding `ipl` onto a disc image: an ISO9660 filesystem
/// with the directory's files in its root, and a cue sheet naming it as a
/// single MODE1/2048 data track, which is what geolith wants to be handed.
///
/// Releases like this ship as the loose files that were meant to go on a CD —
/// the `.PRG`, `.FIX` and `.Z80` the BIOS loads, plus the `IPL.TXT` listing them
/// — because that is what the devkit produced. Nothing in the chain will take
/// them as they are: the core only opens a disc, and the BIOS only reads a
/// filesystem, so the disc has to be made here.
///
/// Only the files directly in that directory go on, since a release keeps its
/// sources and readmes in subdirectories and the boot ROM reads a flat root
/// anyway. Returns the cue.
fn create_neocd_disc(ipl: &Path) -> Result<PathBuf> {
    let dir = ipl.parent().unwrap_or(Path::new("."));

    // Sorted by the on-disc identifier: ISO9660 wants the root directory in
    // that order, and it also makes the image — and so the cache key — the same
    // whatever order the filesystem hands the entries back in.
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        match iso_name(&name) {
            Some(iso) => entries.push((iso, path.clone())),
            // Level 1 is 8.3, and the BIOS reads a level 1 filesystem. A name
            // that doesn't fit is one it could never open, so leaving the file
            // off changes nothing except the size of the image.
            None => debug!("Leaving {name:?} off the disc: not an 8.3 name"),
        }
    }
    entries.sort();

    let mut files = Vec::with_capacity(entries.len());
    for (name, path) in &entries {
        files.push((name.clone(), fs::read(path)?));
    }

    // A file the boot list names but the disc doesn't carry is a hang on the
    // loading screen with nothing to explain it, so say so up front.
    let boot_list = files
        .iter()
        .find(|(name, _)| name == IPL_NAME)
        .map(|(_, data)| String::from_utf8_lossy(data).into_owned())
        .unwrap_or_default();
    for name in ipl_entries(&boot_list) {
        if !files.iter().any(|(on_disc, _)| *on_disc == name) {
            warn!("{IPL_NAME} names {name}, which is not in {dir:?}");
        }
    }

    let present = |name: &'static str| files.iter().any(|(n, _)| n == name).then_some(name);
    // Key the cache on the contents, never the path: a release unpacked from an
    // archive lands in a fresh temp directory every launch, so a path or mtime
    // key would rebuild — and pile up another copy — every time.
    let mut key = KeyHasher::new();
    for (name, data) in &files {
        key.add(name);
        key.add(data);
    }

    let entry = CACHE.get_dir(&key.finish(), DISC_CUE, |out_dir| {
        info!(
            "Building a Neo Geo CD image from {} files in {dir:?}",
            files.len()
        );
        let image = build_iso(&IsoSpec {
            system_id: "NEO-GEO",
            volume_id: "NEOGEOCD",
            files: &files,
            copyright_file: present(COPYRIGHT_FILE),
            abstract_file: present(ABSTRACT_FILE),
            bibliographic_file: present(BIBLIO_FILE),
            pad_sectors: 0,
        });

        fs::write(out_dir.join("disc.iso"), image)?;
        // A single MODE1/2048 track: the sector size the ISO is written in, so
        // the core reads it straight through with no sector translation.
        fs::write(
            out_dir.join(DISC_CUE),
            "FILE \"disc.iso\" BINARY\n  TRACK 01 MODE1/2048\n    INDEX 01 00:00:00\n",
        )?;
        Ok(())
    })?;
    Ok(entry.join(DISC_CUE))
}

/// The cue sheet naming the built image. Written last, so its presence is what
/// tells [`crate::cache::FileCache`] the entry finished.
const DISC_CUE: &str = "disc.cue";

/// Built Neo Geo CD images, keyed on the contents of the directory they were
/// built from. One entry is a single-track ISO of a whole release.
static CACHE: LazyLock<FileCache> = LazyLock::new(|| FileCache::new("neocd"));

const CACHE_LIMIT: u64 = 500 * 1024 * 1024;

pub fn prune_cache() {
    CACHE.prune(CACHE_LIMIT);
}

/// What a `.neo` cartridge ROM opens with: the NeoSD container's tag, ahead of
/// a version byte that is not worth being fussy about.
const NEO_ROM_MAGIC: &[u8] = b"NEO";

pub struct NeoGeoSystem {}

impl System for NeoGeoSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["neo", "cue", "chd"]
    }
    fn is_console(&self) -> bool {
        true
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_GEOLITH
    }
    fn name(&self) -> &'static str {
        "NeoGeo"
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut rom = None;
        let mut cue = None;
        let mut chd = None;
        let mut disc = None;
        let mut ipl = None;

        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.eq_ignore_ascii_case(IPL_NAME) {
                ipl.get_or_insert_with(|| path.to_owned());
            // `.neo` is also what NEOchrome saves an Atari ST picture as (see
            // [`crate::degas`]), so the cartridge's magic has to say so too.
            } else if ext == "neo" && header.starts_with(NEO_ROM_MAGIC) {
                rom.get_or_insert_with(|| path.to_owned());
            } else if ext == "chd" {
                // A CHD is compressed, so there is no cheap way to look inside
                // for the boot list. Nothing else here reads one, so take it.
                chd.get_or_insert_with(|| path.to_owned());
            } else if ext == "cue" {
                if cue.is_none() && is_neogeo_cd_cue(path) {
                    cue = Some(path.to_owned());
                }
            } else if disc.is_none() && TRACK_EXTENSIONS.contains(&ext) && is_neogeo_cd_disc(path) {
                disc = Some(path.to_owned());
            }
            Ok(())
        })?;

        // A release often points at one file inside the disc directory rather
        // than the directory itself — the `.prg` the boot list loads first, say
        // — and the disc is the whole directory it sits in.
        if ipl.is_none()
            && file.path.is_file()
            && let Some(parent) = file.path.parent()
        {
            let beside = parent.join(IPL_NAME);
            if beside.is_file() {
                ipl = Some(beside);
            }
        }

        // The cartridge ROM first: it needs no BIOS disc and boots instantly. A
        // cue describes a whole disc — data track plus any CD audio — so it wins
        // over the track it names, and a disc that exists wins over one that
        // would have to be built.
        let found = match (rom, cue, chd, disc) {
            (Some(rom), ..) => rom,
            (_, Some(cue), ..) => cue,
            (_, _, Some(chd), _) => chd,
            (_, _, _, Some(disc)) => disc,
            _ => match ipl {
                Some(ipl) => create_neocd_disc(&ipl)?,
                None => return Ok(false),
            },
        };

        // A disc boots through the CD BIOS the same way real hardware does, and
        // geolith gives up with only its own message if it isn't there. Say
        // which file is missing while there is still context to say it in.
        if !super::get_ext(&found).eq("neo") {
            const CD_BIOS: &[&str] = &["neocdz.zip", "neocd.zip"];
            let dir = crate::system_dir();
            if !CD_BIOS.iter().any(|name| dir.join(name).is_file()) {
                warn!(
                    "Neo Geo CD needs {} in {dir:?}; the core will not boot without it",
                    CD_BIOS.join(" or ")
                );
            }
        }

        debug!("FMT: Neo Geo {found:?}");
        file.path = found;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The loose files a devkit leaves behind have to come back as a disc the
    /// same detection accepts, or the release would only load the once.
    #[test]
    fn builds_a_disc_from_loose_files() {
        let dir = std::env::temp_dir().join("demarc_neocd_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("IPL.TXT"), "TEST.PRG,0,0\r\nFIX.FIX,0,0\r\n\u{1a}").unwrap();
        fs::write(dir.join("Test.prg"), vec![1u8; 90_000]).unwrap();
        fs::write(dir.join("fix.fix"), vec![2u8; 4096]).unwrap();
        // Neither belongs on the disc: a subdirectory the boot ROM can't reach,
        // and a name that isn't 8.3.
        fs::create_dir_all(dir.join("sources")).unwrap();
        fs::write(dir.join("sources/intro.s"), b"; source").unwrap();
        fs::write(dir.join("a much longer name.txt"), b"readme").unwrap();

        let cue = create_neocd_disc(&dir.join("IPL.TXT")).unwrap();
        assert!(is_neogeo_cd_cue(&cue));

        let mut image = DiscImage::open(&cue.with_file_name("disc.iso")).unwrap();
        assert_eq!(image.root_names(), ["FIX.FIX", "IPL.TXT", "TEST.PRG"]);

        // Same contents, same image — a release unpacked to a new temp
        // directory each launch must not rebuild it.
        assert_eq!(create_neocd_disc(&dir.join("IPL.TXT")).unwrap(), cue);
    }

    /// The boot list is `NAME,BANK,OFFSET` per line, CRLF terminated, and ends
    /// with a DOS EOF byte that is not part of the last name.
    #[test]
    fn reads_the_boot_list() {
        let text = "TEST.PRG,0,0\r\nFIX.FIX,0,0\r\nSOUND9V3.Z80,0,00\r\n\u{1a}";
        assert_eq!(ipl_entries(text), ["TEST.PRG", "FIX.FIX", "SOUND9V3.Z80"]);
    }
}
