use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use tracing::{debug, info, warn};
use url::Url;

use crate::{
    cbmconvert,
    fetch::{fetch_url, fetch_urls},
    frontend::system_dir,
    systems::{GameInfo, SystemType, WorkingFile, get_system_type},
    utils::{
        build_atari_auto_disk, build_m3u, copy_dir_all, has_matching, is_disk_image, is_psx_exe,
        is_self_booting_dir, parse_m3u, prepare_psx_disc, read_header, scan_release_dir,
        sort_disks, unpack_into, unpack_to_temp,
    },
};

/// Where an [`EmuFile`]'s data comes from: either an already-local path or one
/// or more remote URLs that are downloaded on demand (see [`FileSource::resolve`]).
#[derive(Clone, Debug)]
pub enum FileSource {
    Url(Vec<Url>),
    Path(PathBuf),
}

impl Default for FileSource {
    fn default() -> Self {
        FileSource::Path(PathBuf::new())
    }
}

impl From<PathBuf> for FileSource {
    fn from(path: PathBuf) -> Self {
        FileSource::Path(path)
    }
}

impl From<&Path> for FileSource {
    fn from(path: &Path) -> Self {
        FileSource::Path(path.to_owned())
    }
}

/// Extensions that are never the main file of a release.
const IGNORED_EXTENSIONS: [&str; 2] = ["sid", "pdf"];

fn url_extension(url: &Url) -> Option<String> {
    Path::new(url.path())
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Narrow the URLs of one release down to the ones worth downloading.
///
/// A release listing often mixes the actual program with extras (music rips,
/// scans, ...). If any URL is a disk image, the release is disk based and only
/// disk images of that same kind are kept — that way a multi-disk set stays
/// together without dragging in, say, an `.adf` version of a `.d64` release.
/// Otherwise only the obviously non-loadable extras are dropped.
///
/// Filtering never yields an empty list: if everything would be removed the
/// input is returned unchanged, so the caller still has something to fetch.
fn filter_release_urls(urls: Vec<Url>) -> Vec<Url> {
    let disk_ext = urls
        .iter()
        .find(|u| is_disk_image(Path::new(u.path())))
        .and_then(url_extension);

    let filtered: Vec<Url> = match &disk_ext {
        Some(ext) => urls
            .iter()
            .filter(|u| url_extension(u).as_ref() == Some(ext))
            .cloned()
            .collect(),
        None => urls
            .iter()
            .filter(|u| !url_extension(u).is_some_and(|e| IGNORED_EXTENSIONS.contains(&e.as_str())))
            .cloned()
            .collect(),
    };

    if filtered.is_empty() { urls } else { filtered }
}

impl FileSource {
    /// Ensure the data is available locally — downloading the URL (cached, see
    /// [`fetch_url`]) the first time — and return the resulting local path. A
    /// [`FileSource::Path`] is returned as-is.
    fn resolve(&mut self) -> Result<&PathBuf> {
        if let FileSource::Url(urls) = self {
            // If any URL is a disk image, this is a (possibly multi-) disk set:
            // download every disk image so they sit together in one directory
            // (built into an m3u later). Otherwise just grab the first entry.
            let urls = filter_release_urls(urls.clone());
            let p = if urls.iter().any(|u| is_disk_image(Path::new(u.path()))) {
                fetch_urls(&urls)?
            } else {
                fetch_url(urls.first().unwrap().as_ref())?
            };
            *self = FileSource::Path(p);
        }
        match self {
            FileSource::Path(p) => Ok(p),
            FileSource::Url(_) => unreachable!("just converted to Path above"),
        }
    }

    /// A cheap, read-only path view for extension checks or display names: the
    /// local path directly, or a URL rendered as a path. The result may not
    /// exist on disk — use [`resolve`](Self::resolve) to obtain a real local
    /// file for a URL.
    pub fn as_path(&self) -> Cow<'_, Path> {
        match self {
            FileSource::Path(p) => Cow::Borrowed(p),
            FileSource::Url(u) => Cow::Owned(PathBuf::from(
                u.first().map(Url::as_str).unwrap_or_default(),
            )),
        }
    }
}

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
    pub path: FileSource,
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
        path: path.into(),
        system_type,
        tags,
        game_info: GameInfo {
            title,
            group,
            year,
            typ: "".into(),
        },
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

    let mut is_bitworld = false;

    for l in text.lines() {
        let line = l.trim();

        if line.is_empty() || line.starts_with('#') {
            is_bitworld = true;
            continue;
        }
        let mut fields = line.split('\t');
        let mut next = || fields.next().unwrap_or_default();
        let _id = next();
        let title = next().to_string();
        let group = next().to_string();
        let date = next();
        let party = next();
        let demo_type = next();
        if demo_type.ends_with("disk") {
            continue;
        }
        let demo_type = if is_bitworld {
            format!("Amiga {demo_type}")
        } else {
            demo_type.to_string()
        };
        let tag_list = fields.next().unwrap_or_default();
        let url = fields.next().unwrap_or_default().trim();
        if url.is_empty() {
            continue;
        }
        let urls: Vec<Url> = url
            .split(';')
            .filter_map(|p| match Url::parse(p) {
                Ok(u) => Some(u),
                Err(err) => {
                    warn!("Skipping unparseable URL {p:?}: {err}");
                    None
                }
            })
            .collect();
        if urls.is_empty() {
            continue;
        }

        let year = date
            .split(['-', '/', '.'])
            .next()
            .unwrap_or_default()
            .to_string();
        let mut tags = HashMap::new();
        for (key, val) in [("party", party), ("type", &demo_type), ("tags", tag_list)] {
            if !val.is_empty() {
                tags.insert(key.to_string(), val.to_string());
            }
        }

        out.push(EmuFile {
            path: FileSource::Url(urls),
            system_type: SystemType::Unknown,
            tags,
            game_info: GameInfo {
                title,
                group,
                year,
                typ: demo_type,
            },
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
            path: in_path.into(),
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

/// Extension check shared by the conversion helpers below. Content sniffing is
/// left to [`unpack_into`]: the file has to be picked out by name first, since
/// what conversion does to it — replacing it in place — must never be a
/// surprise for a file sitting in the user's own directory.
fn has_extension(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Wrappers that hold a release rather than being one: a scener's zip often has
/// the disk image inside a `.gz`, and a `.tar.gz` puts another layer on top of
/// that. Expanding them in place is always right — unlike a nested zip or lha,
/// which is usually a second, alternative copy of the release.
fn is_wrapper(path: &Path) -> bool {
    ["gz", "tgz", "bz2", "z", "tar"]
        .iter()
        .any(|ext| has_extension(path, ext))
}

/// True if `path` names a container that isn't loadable as-is and that
/// [`convert_dir`] knows how to turn into something that is.
fn needs_conversion(path: &Path) -> bool {
    has_extension(path, "t64") || is_wrapper(path)
}

/// True if `path` — a file, or anything in a directory tree — needs converting.
fn has_convertible(path: &Path) -> bool {
    if path.is_dir() {
        fs::read_dir(path)
            .is_ok_and(|entries| entries.flatten().any(|e| has_convertible(&e.path())))
    } else {
        needs_conversion(path)
    }
}

/// How many times [`convert_dir`] re-scans for wrappers. Two rounds cover the
/// `.tar.gz` case; the limit is only there so a self-referential chain can't
/// spin forever.
const MAX_EXPAND_PASSES: usize = 8;

/// Expand every wrapper under `dir` in place, replacing each one with what it
/// held. Returns whether anything was expanded.
fn expand_wrappers(dir: &Path) -> Result<bool> {
    let mut expanded = false;
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            expanded |= expand_wrappers(&path)?;
        } else if is_wrapper(&path) {
            // A `.gz` yields its payload under the archive's stem, so
            // `demo.adf.gz` becomes `demo.adf` right next to it.
            match unpack_into(&path, dir) {
                // Drop the wrapper once its contents are out, so the file
                // counting further down sees only the real files.
                Ok(true) => {
                    debug!("FMT: expanded {path:?}");
                    fs::remove_file(&path)?;
                    expanded = true;
                }
                Ok(false) => debug!("FMT: {path:?} is not an archive after all"),
                Err(err) => warn!("Could not expand {path:?}: {err}"),
            }
        }
    }
    Ok(expanded)
}

/// Turn everything under `dir` that isn't loadable on its own into something
/// that is: wrappers (`.gz` and friends) are expanded in place, and `.t64` tape
/// images are handed to cbmconvert, which splits them into raw `.prg` files.
///
/// Both write next to the file they came from — and expanding deletes it — so
/// `dir` has to be a directory we own; see [`stage_for_convert`].
fn convert_dir(dir: &Path) -> Result<()> {
    // Expanding can uncover more wrappers (`.tar.gz` -> `.tar` -> the files),
    // so keep going until a pass finds nothing left.
    for pass in 0.. {
        if !expand_wrappers(dir)? {
            break;
        }
        if pass + 1 >= MAX_EXPAND_PASSES {
            warn!("Giving up expanding archives in {dir:?} after {MAX_EXPAND_PASSES} passes");
            break;
        }
    }
    convert_files(dir)
}

/// The cbmconvert half of [`convert_dir`], kept separate so it runs once per
/// file rather than once per expansion pass.
fn convert_files(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            convert_files(&path)?;
        } else if has_extension(&path, "t64") {
            info!("Converting {path:?}");
            let _guard = cbmconvert::CwdGuard::enter(dir);
            let code = cbmconvert::run(["-t", "-N", path.to_string_lossy().as_ref()]);
            if code != 0 {
                warn!("cbmconvert failed on {path:?} (exit code {code})");
            }
        }
    }
    Ok(())
}

/// Give conversion a directory of its own to work in.
///
/// Since conversion drops its output next to the input, anything still holding
/// a convertible file is copied into a fresh temp directory first: a lone file
/// on its own — so it takes the same route as an unpacked archive from here on
/// — a directory as a whole tree. An `already_temp` path is ours to write to
/// and is converted in place. Returns `None` when there is nothing to convert,
/// in which case the caller keeps using the path it has.
fn stage_for_convert(path: &Path, already_temp: bool) -> Result<Option<PathBuf>> {
    if already_temp || !has_convertible(path) {
        return Ok(None);
    }
    let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
    if path.is_dir() {
        copy_dir_all(path, &dir)?;
    } else {
        fs::copy(path, dir.join(path.file_name().unwrap()))?;
    }
    debug!("FMT: staged {path:?} for conversion in {dir:?}");
    Ok(Some(dir))
}

/// Append every file under `dir` to `out`, stopping as soon as there is more
/// than one — the only question [`only_file`] asks.
fn gather_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            gather_files(&path, out)?;
        } else {
            out.push(path);
        }
        if out.len() > 1 {
            return Ok(());
        }
    }
    Ok(())
}

/// The single file in `dir`'s tree, or `None` if it holds none or several. A
/// directory that boils down to one file is really that file, even when its
/// type wasn't recognized during the scan — the content checks further down
/// still get a shot at identifying it.
fn only_file(dir: &Path) -> Option<PathBuf> {
    let mut files = vec![];
    gather_files(dir, &mut files).ok()?;
    match files.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// Prepare a file for loading into emulator. Unpack archives, parse
/// binaries for more info, convert formats as needed.
/// Also need to determine system_type if not done previously
pub fn prepare_file(emu_file: &EmuFile) -> Result<WorkingFile> {
    let EmuFile {
        mut system_type,
        path: mut source,
        mut tags,
        game_info,
    } = emu_file.clone();
    let mut is_temp = false;

    // Entries backed by a URL (e.g. from a `--db` list) are downloaded to the
    // local cache on first load, then handled like any other local file. The
    // cache means later loads of the same URL are free. Re-detect the system
    // type once we have the real file, since collection only saw the URL.
    let was_url = matches!(source, FileSource::Url(_));
    let mut path = source.resolve()?.clone();
    if was_url {
        system_type = get_system_type(&path);
    }

    if path.is_file() {
        if let Some(unpacked) = unpack_to_temp(&path)? {
            debug!("FMT: unpacked archive {path:?} -> {unpacked:?}");
            path = unpacked;
            is_temp = true;
            system_type = get_system_type(&path);
        }
    }

    // A file that needs converting becomes a temp directory holding just that
    // file, so from here on there is one route: convert the directory, then
    // pick the file back out of it below.
    if let Some(staged) = stage_for_convert(&path, is_temp)? {
        path = staged;
        is_temp = true;
    }
    let mut copy_all = false;

    if path.is_dir() {
        if let Err(err) = convert_dir(&path) {
            warn!("Conversion failed in {path:?}: {err}");
        }

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
            } else if let Some(f) = scan.first_file.or_else(|| only_file(&path)) {
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
                let path = f.path.as_path();
                let rel = path.strip_prefix(root).unwrap_or(&path);
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

    /// A lone `.t64` isn't loadable, so it is staged in a temp directory,
    /// converted there, and the resulting `.prg` is what comes out — with the
    /// tape image's own directory left exactly as it was.
    #[test]
    fn t64_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let before = fs::read_dir(root.join("testdata")).unwrap().count();
        let wf = prepare("testdata/BADALM.T64");
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.extension().unwrap(), "prg");
        assert!(wf.is_temp);
        assert!(!wf.path.starts_with(root), "converted next to the source");
        assert_eq!(fs::read_dir(root.join("testdata")).unwrap().count(), before);
    }

    /// Scene zips often hold each disk gzipped. Both layers have to come off
    /// before the release reads as the two-disk set it is.
    #[test]
    fn zip_of_gzipped_disks() {
        let wf = prepare("testdata/Skaaneland.zip");
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.extension().unwrap(), "m3u");
        let m3u = fs::read_to_string(&wf.path).unwrap();
        assert_eq!(
            m3u.lines().filter(|l| l.ends_with(".d64")).count(),
            2,
            "both disks should be in the playlist, got {m3u:?}"
        );
    }

    /// A wrapper around something that itself needs converting: the `.gz` comes
    /// off first, then cbmconvert turns the tape image into a `.prg`.
    #[test]
    fn gzipped_t64() {
        let wf = prepare("testdata/Maniacs of Noise Logo.t64.gz");
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.extension().unwrap(), "prg");
        assert!(wf.is_temp);
    }

    /// Same for a directory holding one: it is copied before conversion, so
    /// the `.prg` lands in the copy and the original directory is untouched.
    #[test]
    fn t64_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            root.join("testdata/BADALM.T64"),
            dir.path().join("BADALM.T64"),
        )
        .unwrap();

        let wf = prepare_file(&EmuFile {
            path: dir.path().into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.extension().unwrap(), "prg");
        assert!(!wf.path.starts_with(dir.path()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
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
        assert_eq!(
            &*eod.path.as_path(),
            Path::new("https://example.com/eod.d64")
        );
        // The system type is only determined once the URL is fetched (see
        // `prepare_file`), so collection leaves it Unknown.
        assert_eq!(eod.system_type, SystemType::Unknown);
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

    fn filter(urls: &[&str]) -> Vec<String> {
        let parsed = urls.iter().map(|u| Url::parse(u).unwrap()).collect();
        filter_release_urls(parsed)
            .iter()
            .map(Url::to_string)
            .collect()
    }

    /// A disk image among the URLs makes the release disk based: the extras and
    /// any disk image of another kind go away, the rest of the set stays.
    #[test]
    fn disk_images_win() {
        assert_eq!(
            filter(&[
                "https://x.com/a.pdf",
                "https://x.com/a1.d64",
                "https://x.com/a2.D64",
                "https://x.com/a.adf",
                "https://x.com/readme.txt",
            ]),
            vec!["https://x.com/a1.d64", "https://x.com/a2.D64"]
        );
    }

    /// Without a disk image only the known extras are dropped; everything else,
    /// including extension-less URLs, is left for the caller to sort out.
    #[test]
    fn extras_dropped() {
        assert_eq!(
            filter(&[
                "https://x.com/demo.sid",
                "https://x.com/demo.zip",
                "https://x.com/scan.PDF",
                "https://x.com/download.php?id=1",
            ]),
            vec!["https://x.com/demo.zip", "https://x.com/download.php?id=1"]
        );
    }

    /// Filtering everything away would leave nothing to fetch, so the input
    /// survives untouched instead.
    #[test]
    fn all_filtered_keeps_input() {
        assert_eq!(
            filter(&["https://x.com/a.sid", "https://x.com/b.pdf"]),
            vec!["https://x.com/a.sid", "https://x.com/b.pdf"]
        );
    }

    /// A directory of `.prg`s mixed with `.png` previews: every program is
    /// collected on its own and prepares as a plain C64 file.
    #[test]
    fn prg_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![];
        collect_files(&root.join("testdata/intros"), &mut out, true).unwrap();
        assert_eq!(out.len(), 15);
        out.sort_by(|a, b| a.path.as_path().cmp(&b.path.as_path()));
        let wf = prepare_file(&out[0]).unwrap();
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.as_path(), &*out[0].path.as_path());
    }
}
