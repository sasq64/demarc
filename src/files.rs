use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use tracing::{debug, info, warn};

use crate::{
    frontend::system_dir,
    systems::{GameInfo, SystemType, WorkingFile, get_system_type},
    utils::{
        build_atari_auto_disk, build_m3u, copy_dir_all, has_matching, is_disk_image, is_lha_file,
        is_psx_exe, is_self_booting_dir, is_zip_file, parse_m3u, prepare_psx_disc, read_header,
        scan_release_dir, sort_disks, unlha_to_temp, unzip_to_temp,
    },
};

// EmuFile can be:
// * Single PRG, ADF or other
// * Parsed M3U for loading
//   - Amiga or C64 with disks listed, path = m3u
// * Parsed M3U but not supported for loading
//   - No files listed, path = directory
// * Directory (if leaf)
// * Archive (sytem type unknown)

#[derive(Default, Clone, Debug)]
pub struct EmuFile {
    pub path: PathBuf,
    pub tags: HashMap<String, String>,
    pub system_type: SystemType,
    pub game_info: GameInfo,
}

fn handle_m3u(in_path: &Path) -> Result<EmuFile> {
    let mut title: String = "".into();
    let mut group: String = "".into();
    let mut year: String = "".into();
    let mut system_type = SystemType::Unknown;
    let mut tags = HashMap::new();

    let m3u = parse_m3u(in_path)?;
    for f in &m3u.files {
        let t = get_system_type(f);
        if system_type != SystemType::Unknown && t != system_type {
            // Mixed
            system_type = SystemType::Unknown;
            break;
        }
        system_type = t;
    }

    let path = (if m3u.files.is_empty() {
        in_path.parent().unwrap()
    } else {
        in_path
    })
    .to_owned();

    info!("{:?}", m3u.tags);
    for (key, val) in m3u.tags {
        if key == "title" {
            title = val.clone();
        } else if key == "group" {
            group = val.clone();
        } else if key == "year" {
            year = val.clone();
        } else {
            tags.insert(key, val);
        }
    }
    Ok(EmuFile {
        path,
        system_type,
        tags,
        game_info: GameInfo { title, group, year },
    })
}

/// Parse a tab-separated demo database file into `EmuFile` entries appended to
/// `out`.
///
/// Each non-blank line holds the fields `id, title, group, date, party, type,
/// tags, url` separated by tabs. The `url` becomes the entry's path and is
/// downloaded on demand the first time it's loaded (see [`prepare_file`]); the
/// year is the first `-`/`/`/`.`-delimited part of `date`. The remaining scene
/// metadata (`party`, `type`, `tags`) is kept under matching keys. Lines with
/// no URL are skipped.
pub fn collect_db(path: &Path, out: &mut Vec<EmuFile>) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => bail!("Failed to read db file {}: {err}", path.display()),
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut fields = line.split('\t');
        let _id = fields.next().unwrap_or_default();
        let title = fields.next().unwrap_or_default().to_string();
        let group = fields.next().unwrap_or_default().to_string();
        let date = fields.next().unwrap_or_default();
        let party = fields.next().unwrap_or_default();
        let demo_type = fields.next().unwrap_or_default();
        let tag_list = fields.next().unwrap_or_default();
        let url = fields.next().unwrap_or_default().trim();
        if url.is_empty() {
            continue;
        }

        let year = date
            .split(['-', '/', '.'])
            .next()
            .unwrap_or_default()
            .to_string();
        let mut tags = HashMap::new();
        for (key, val) in [("party", party), ("type", demo_type), ("tags", tag_list)] {
            if !val.is_empty() {
                tags.insert(key.to_string(), val.to_string());
            }
        }

        out.push(EmuFile {
            path: PathBuf::from(url),
            system_type: get_system_type(Path::new(url)),
            tags,
            game_info: GameInfo { title, group, year },
        });
    }
    Ok(())
}

pub fn collect_file(in_path: &Path) -> Result<EmuFile> {
    if in_path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
    {
        handle_m3u(in_path)
    } else {
        let title = in_path.file_stem().unwrap().to_string_lossy().to_string();
        Ok(EmuFile {
            path: in_path.to_owned(),
            system_type: get_system_type(in_path),
            game_info: GameInfo {
                title,
                ..Default::default()
            },
            ..Default::default()
        })
    }
}

/// Recursively collect all detected emulator files under `dir` into `out`.
/// Does not unpack archives, but collects metadata from m3u files
/// Rule-of-thumb: Collect only cheap information, since there can be a lot of files
pub fn collect_files(dir: &Path, out: &mut Vec<EmuFile>, many: bool) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            bail!("Failed to read directory {}: {err}", dir.display());
        }
    };
    let mut files = vec![];
    let mut dirs = vec![];
    let mut found_type = SystemType::Unknown;
    let mut mixed = many;
    let mut disk_images = true;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
        {
            // We found an m3u, don't recurse further
            out.push(handle_m3u(&path)?);
            return Ok(());
        } else {
            let t = get_system_type(&path);
            if t != SystemType::Unknown {
                if found_type != SystemType::Unknown && found_type != t {
                    mixed = true;
                }
                if !is_disk_image(&path) {
                    disk_images = false;
                }
                found_type = t;
                files.push(path);
            }
        }
    }

    // Mixed types in directory, add every valid file one by one
    // Amiga: Always add parent dir (to get data files)
    if mixed || ((!disk_images) && found_type != SystemType::Amiga) {
        //out.extend(files.iter().map(|f| handle_file(f)?));
        for f in files {
            out.push(collect_file(&f)?);
        }
    } else if found_type != SystemType::Unknown {
        out.push(collect_file(dir)?);
        return Ok(());
    }
    for dir in dirs {
        collect_files(&dir, out, many)?;
    }
    Ok(())
}

/// Prepare a file for loading into emulator. Unpack archives, parse
/// binaries for more info, convert formats as needed.
/// Also need to determine system_type if not done previously
pub fn prepare_file(emu_file: &EmuFile) -> Result<WorkingFile> {
    let EmuFile {
        mut system_type,
        mut path,
        mut tags,
        game_info,
    } = emu_file.clone();
    let mut is_temp = false;

    // Entries whose path is an http(s):// URL (e.g. from a `--db` list) are
    // downloaded to the local cache on first load, then handled like any other
    // local file. The cache means later loads of the same URL are free.
    if let Some(url) = path.to_str().filter(|s| crate::fetch::is_url(s)) {
        path = crate::fetch::fetch_url(url)?;
        system_type = get_system_type(&path);
    }

    if path.is_file() && is_zip_file(&path) {
        debug!("FMT: zip archive");
        path = unzip_to_temp(&path)?;
        is_temp = true;
        system_type = get_system_type(&path);
    } else if path.is_file() && is_lha_file(&path) {
        debug!("FMT: lha archive");
        path = unlha_to_temp(&path)?;
        is_temp = true;
        system_type = get_system_type(&path);
    }
    let mut copy_all = false;

    if path.is_dir() {
        if is_self_booting_dir(&path) {
            debug!("FMT: Amiga self-booting");
            system_type = SystemType::Amiga;
            tags.insert("puae_use_whdload".into(), "disabled".into());
        } else if has_matching(&path, ".slave").is_some() {
            debug!("FMT: Amiga WHDLoad");
            system_type = SystemType::Amiga;
            tags.insert("puae_model".into(), "A1200".into());
            tags.insert("puae_use_whdload".into(), "enabled".into());
        } else {
            let scan = scan_release_dir(&path)?;
            if scan.system_type != SystemType::Unknown {
                system_type = scan.system_type;
            }
            let mut files = scan.disk_images;
            if files.len() > 1 {
                sort_disks(&mut files);
                path = build_m3u(&files)?;
                is_temp = true;
            } else if let Some(f) = scan.first_file {
                path = f;
                copy_all = true;
            }
        }
    }

    if !path.is_dir() {
        if is_psx_exe(&path) {
            tags.insert("psx_core".into(), "beetle".into());
        }

        // Only the first four bytes are examined below; the Amiga branch works
        // off `path`, not the contents.
        let data = read_header(&path, 4)?;
        if data.len() >= 2 && data[0..2] == [0x60, 0x1a] {
            // GEMDOS executable: wrap it in a bootable Atari ST floppy image
            // with the program in the AUTO folder so it runs on boot. The whole
            // executable goes on the disk, not just the header read above.
            path = build_atari_auto_disk(&fs::read(&path)?)?;
            is_temp = true;
            system_type = SystemType::AtariST;
        } else if data.len() >= 2 && data[0..2] == [0x01, 0x08] {
            system_type = SystemType::C64;
        } else if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
            debug!("FMT: Amiga exe: {path:?}");
            if std::fs::metadata(&path)?.len() > 850 * 1024 {
                tags.insert("puae_model".into(), "A1200".into());
            }
            let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
            let s_dir = target_dir.join("s");
            fs::create_dir(&s_dir)?;
            let c_dir = target_dir.join("c");
            fs::create_dir(&c_dir)?;
            fs::copy(system_dir().join("c").join("echo"), c_dir.join("echo"))?;
            let mut text: String = "".into();
            let model = tags.get("puae_model").map_or("", |s| s.as_str());
            if model == "A1200" || model == "A4000" {
                fs::copy(
                    system_dir().join("c").join("SetPatch"),
                    c_dir.join("SetPatch"),
                )?;
                text += "SetPatch QUIET\n";
            }
            if copy_all {
                let name = path.file_name().unwrap().to_str().unwrap();
                text += &format!("echo \"Loading...\"\n{name}\n");
            } else {
                text += "echo \"Loading...\"\namiga_file\n";
            }
            fs::write(s_dir.join("startup-sequence"), text)?;
            if copy_all {
                copy_dir_all(path.parent().unwrap(), &target_dir)?;
            } else {
                fs::copy(&path, target_dir.join("amiga_file"))?;
            }
            path = target_dir;
            is_temp = true;
            tags.insert("puae_use_whdload".into(), "disabled".into());
            system_type = SystemType::Amiga;
        }
    };

    // The rewritten disc lives in the cache and is reused across runs, so it is
    // deliberately not marked temp — `WorkingFile`'s drop must not delete it.
    if system_type == SystemType::Psx
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("cue"))
    {
        match prepare_psx_disc(&path) {
            Ok(Some(rewritten)) => path = rewritten,
            Ok(None) => {}
            Err(err) => warn!("Could not prepare PSX disc {path:?}: {err}"),
        }
    }
    Ok(WorkingFile {
        system_type,
        path,
        settings: tags,
        game_info,
        is_temp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn load_demo() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![];
        collect_files(&root.join("demos/nexus7"), &mut out, false).unwrap();
        println!("{:?}", out[0]);
        let wf = prepare_file(&out[0]);
        println!("{:?}", wf);
    }

    /// Collect `<crate root>/<dir>`, with paths made relative again so the
    /// assertions below don't depend on where the checkout lives. `read_dir`
    /// order is unspecified, so everything is sorted before comparing.
    fn collect(dir: &str, many: bool) -> Vec<(String, SystemType)> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![];
        collect_files(&root.join(dir), &mut out, many).unwrap();
        let mut found: Vec<(String, SystemType)> = out
            .iter()
            .map(|f| {
                let rel = f.path.strip_prefix(root).unwrap_or(&f.path);
                (rel.to_string_lossy().into_owned(), f.system_type)
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    fn type_of(found: &[(String, SystemType)], path: &str) -> SystemType {
        found
            .iter()
            .find(|(p, _)| p == path)
            .unwrap_or_else(|| panic!("{path} not collected, got {found:#?}"))
            .1
    }

    /// The demos dir mixes systems, so every file is listed on its own. The two
    /// subdirectories don't expand into files: the Amiga demo needs its whole
    /// directory (for the data files next to the executable), and nexus7's
    /// `.m3u` carries only metadata, so it stands in for the directory.
    #[test]
    fn collects_demos() {
        let found = collect("demos", false);
        assert_eq!(
            found,
            vec![
                ("demos/natrium.prg".into(), SystemType::AtariST),
                ("demos/nexus7".into(), SystemType::Unknown),
                ("demos/nightmode.gb".into(), SystemType::Gameboy),
                ("demos/o2-intro".into(), SystemType::Amiga),
                ("demos/pdx-dlcm.psx".into(), SystemType::Psx),
                ("demos/quantum_icc2026_v1p.prg".into(), SystemType::C64),
                ("demos/rebels.adf".into(), SystemType::Amiga),
                ("demos/still_rising.dsk".into(), SystemType::Amstrad),
                ("demos/triad.smc".into(), SystemType::SuperNintendo),
            ]
        );
    }

    /// A directory holding an `.m3u` is represented by that playlist alone; the
    /// files next to it must not also show up as entries of their own. This one
    /// lists no files, so the entry is the directory rather than the playlist —
    /// the whole WHDLoad install is what gets loaded.
    #[test]
    fn m3u_replaces_its_directory() {
        let found = collect("demos", false);
        assert_eq!(
            found
                .iter()
                .filter(|(p, _)| p.starts_with("demos/nexus7"))
                .count(),
            1
        );
        assert_eq!(type_of(&found, "demos/nexus7"), SystemType::Unknown);
    }

    /// `many` splits a directory of same-system files into separate entries,
    /// so the Amiga demo dir becomes the executable inside it.
    #[test]
    fn many_expands_amiga_directory() {
        let found = collect("demos", true);
        assert_eq!(type_of(&found, "demos/o2-intro/o2intro"), SystemType::Amiga);
        assert!(!found.iter().any(|(p, _)| p == "demos/o2-intro"));
    }

    /// testdata is recursed into: the C64 intros (mixed with `.png` previews)
    /// and the IFF images all come out individually, while the archives at the
    /// top level are not recognised as loadable files at all.
    #[test]
    fn collects_testdata() {
        let found = collect("testdata", false);

        assert_eq!(type_of(&found, "testdata/test.iff"), SystemType::Ilbm);
        assert_eq!(
            type_of(&found, "testdata/intros/0/007-01.prg"),
            SystemType::C64
        );
        assert_eq!(type_of(&found, "testdata/iffILBM/24.iff"), SystemType::Ilbm);

        // Detection is by content as well as extension, so extension-less and
        // oddly-named IFFs still count.
        assert_eq!(type_of(&found, "testdata/iffILBM/ghost"), SystemType::Ilbm);
        assert_eq!(
            type_of(&found, "testdata/iffILBM/firestarter.256"),
            SystemType::Ilbm
        );

        for (path, system_type) in &found {
            assert!(!path.ends_with(".png"), "preview image collected: {path}");
            assert!(
                !(path.ends_with(".zip") || path.ends_with(".lha")),
                "archive collected: {path}"
            );
            assert_ne!(*system_type, SystemType::Unknown, "{path}");
        }

        // No `.m3u` anywhere here, so nothing collapses to a directory.
        for (path, _) in &found {
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(path).is_file(),
                "expected a file, got {path}"
            );
        }
    }

    /// `many` makes no difference for testdata — every directory in it is
    /// already mixed or non-Amiga.
    #[test]
    fn testdata_is_unaffected_by_many() {
        assert_eq!(collect("testdata", false), collect("testdata", true));
    }

    #[test]
    fn missing_directory_is_an_error() {
        let mut out = vec![];
        assert!(collect_files(Path::new("no/such/dir"), &mut out, false).is_err());
        assert!(out.is_empty());
    }

    /// Take `<crate root>/<rel>` all the way through both stages, the way the
    /// frontend does: collect it, then unpack/convert it for the emulator.
    fn prepare(rel: &str) -> WorkingFile {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        prepare_file(&collect_file(&root.join(rel)).unwrap()).unwrap()
    }

    /// A bare GEMDOS executable is wrapped in a bootable floppy image, so the
    /// prepared path is no longer the `.prg` handed in.
    #[test]
    fn atari_exe() {
        let wf = prepare("demos/natrium.prg");
        assert_eq!(wf.system_type, SystemType::AtariST);
        assert_eq!(wf.path.extension().unwrap(), "st");
        assert!(wf.is_temp);
    }

    /// The playlist's `#EXTINF` tags become emulator settings.
    #[test]
    fn amiga_m3u() {
        let wf = prepare("demos/nexus7/demo.m3u");
        assert_eq!(wf.settings.get("puae_model").unwrap(), "A1200");
        assert_eq!(wf.system_type, SystemType::Amiga);
    }

    /// An Amiga executable can't be booted directly: it goes into a temp
    /// directory with a generated `startup-sequence` that runs it.
    #[test]
    fn amiga_exe() {
        let wf = prepare("demos/o2-intro/o2intro");
        assert_eq!(wf.system_type, SystemType::Amiga);
        assert!(wf.path.join("s/startup-sequence").exists());
        assert!(wf.path.join("amiga_file").exists());
    }

    #[test]
    fn amiga_lha() {
        let wf = prepare("testdata/vS10-ami.lha");
        assert_eq!(wf.system_type, SystemType::Amiga);
        assert!(wf.path.join("s/startup-sequence").exists());
    }

    /// The archive holds previews and text alongside the cart, so unpacking has
    /// to pick the one loadable file out of the directory.
    #[test]
    fn zip_with_extra_files() {
        let wf = prepare("testdata/gigabates_Terrain-Spotting.zip");
        assert_eq!(wf.system_type, SystemType::Tic80);
        assert_eq!(wf.path.extension().unwrap(), "tic");
    }

    /// An image needs no preparation at all — the path must survive untouched.
    #[test]
    fn ilbm_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let wf = prepare("testdata/test.iff");
        assert_eq!(wf.system_type, SystemType::Ilbm);
        assert_eq!(wf.path, root.join("testdata/test.iff"));
    }

    /// A tab-separated db line becomes an `EmuFile` whose path is the URL and
    /// whose metadata is split out of the fields (year from the date, scene tags
    /// preserved). Blank lines and URL-less lines are skipped.
    #[test]
    fn collect_db_parses_fields() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demos.txt");
        fs::write(
            &db,
            "1\tEdge of Disgrace\tBooze Design\t2008-04-01\tBreakpoint\tdemo\tc64,fav\thttps://example.com/eod.d64\n\
             \n\
             2\tNo URL\tGroup\t1994\tParty\tintro\t\t\n\
             3\tNexus 7\tAndromeda\t1994/12/30\t\t\t\thttps://example.com/nexus7.zip\n",
        )
        .unwrap();

        let mut out = vec![];
        collect_db(&db, &mut out).unwrap();
        assert_eq!(out.len(), 2, "blank and URL-less lines skipped");

        let eod = &out[0];
        assert_eq!(eod.path, PathBuf::from("https://example.com/eod.d64"));
        assert_eq!(eod.system_type, SystemType::C64);
        assert_eq!(eod.game_info.title, "Edge of Disgrace");
        assert_eq!(eod.game_info.group, "Booze Design");
        assert_eq!(eod.game_info.year, "2008");
        assert_eq!(eod.tags.get("party").unwrap(), "Breakpoint");
        assert_eq!(eod.tags.get("type").unwrap(), "demo");
        assert_eq!(eod.tags.get("tags").unwrap(), "c64,fav");

        let nexus = &out[1];
        assert_eq!(nexus.game_info.title, "Nexus 7");
        assert_eq!(nexus.game_info.year, "1994");
        assert!(nexus.tags.is_empty(), "empty scene fields left out");
    }

    /// A directory of `.prg`s mixed with `.png` previews: every program is
    /// collected on its own and prepares as a plain C64 file.
    #[test]
    fn prg_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![];
        collect_files(&root.join("testdata/intros"), &mut out, true).unwrap();
        assert_eq!(out.len(), 15);
        out.sort_by(|a, b| a.path.cmp(&b.path));
        let wf = prepare_file(&out[0]).unwrap();
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path, out[0].path);
    }
}
