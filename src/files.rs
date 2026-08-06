use std::{
    collections::HashMap,
    fs,
    io::{self, IsTerminal, Read},
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use regex::Regex;
use tempfile::TempDir;
use tracing::{debug, info, warn};
use url::Url;

use crate::{
    cbmconvert,
    fetch::{fetch_url, fetch_urls},
    frontend::system_dir,
    systems::{GEMDOS_MAGIC, GameInfo, SystemType, WorkingFile, get_system_type, is_atari_program},
    utils::{
        GBA_HEADER_LEN, build_atari_auto_disk, build_m3u, copy_dir_all, create_psx_iso, find_child,
        fix_psx_text_size, has_matching, is_disk_image, is_gba_rom, is_psx_exe,
        is_self_booting_dir, parse_m3u, prepare_psx_disc, psx_needs_text_fix, read_header,
        scan_release_dir, sort_disks, unpack_if_packed, unpack_into, unpack_to_temp,
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
fn filter_release_urls(urls: Vec<Url>) -> Vec<Url> {
    /// Extensions that are never the main file of a release.
    const IGNORED_EXTENSIONS: [&str; 2] = ["sid", "pdf"];

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

/// The scene metadata one db line carries, however the line spells it out.
#[derive(Default)]
struct DbRecord<'a> {
    title: &'a str,
    group: &'a str,
    date: &'a str,
    party: &'a str,
    demo_type: &'a str,
    tag_list: &'a str,
    url: &'a str,
    /// Platform prefixed to the type, when the line names one itself.
    platform: Option<&'a str>,
}

/// Map a named field onto the [`DbRecord`] slot it fills, or `None` if the name
/// isn't one we know (`id`, for instance, is parsed but unused).
fn db_field<'a, 'r>(rec: &'r mut DbRecord<'a>, key: &str) -> Option<&'r mut &'a str> {
    Some(match key {
        "title" => &mut rec.title,
        "author" | "group" => &mut rec.group,
        "date" => &mut rec.date,
        "party" => &mut rec.party,
        "category" | "type" => &mut rec.demo_type,
        "tags" => &mut rec.tag_list,
        "download" | "url" => &mut rec.url,
        _ => return None,
    })
}

/// Parse a `key:value`-per-field line, e.g.
/// `id:1\ttitle:Zentro 4\tauthor:Zenith\t…`. Returns `None` when the line isn't
/// in that format, so the caller can fall back to the positional one.
///
/// Only the first `:` splits a field, leaving values (URLs above all) intact,
/// and fields may appear in any order.
fn parse_named_db_line(line: &str) -> Option<DbRecord<'_>> {
    let mut rec = DbRecord::default();
    let mut named = false;
    for field in line.split('\t') {
        let Some((key, val)) = field.split_once(':') else {
            continue;
        };
        if key == "platform" {
            rec.platform = Some(val);
            named = true;
        } else if let Some(slot) = db_field(&mut rec, key) {
            *slot = val;
            named = true;
        } else if key == "id" {
            named = true;
        }
    }
    named.then_some(rec)
}

/// Parse the older order-based line: `id, title, group, date, party, type,
/// tags, url` separated by tabs.
fn parse_positional_db_line(line: &str) -> DbRecord<'_> {
    let mut fields = line.split('\t');
    let mut next = || fields.next().unwrap_or_default();
    let _id = next();
    DbRecord {
        title: next(),
        group: next(),
        date: next(),
        party: next(),
        demo_type: next(),
        tag_list: next(),
        url: next(),
        platform: None,
    }
}

/// Parse a tab-separated demo database file into `EmuFile` entries appended to
/// `out`.
///
/// Each non-blank line holds the fields `id, title/author (group), date, party,
/// category (type), tags, download (url)`, either prefixed with their names
/// (`title:Zentro 4`, any order) or, in older db files, in that fixed order.
/// The `url` becomes the entry's path and is downloaded on demand the first
/// time it's loaded (see [`prepare_file`]); the year is the first `-`/`/`/`.`
/// -delimited part of `date`. The remaining scene metadata (`party`, `type`,
/// `tags`) is kept under matching keys. Lines with no URL are skipped.
///
/// A `platform` field, or a `# Platform:<name>` header line applying to every
/// line below it, prefixes the type (`Demo` → `Amiga Demo`). A header line may
/// carry further `key:value` pairs (`# Platform:Amiga puae_model:A500`), which
/// become tags on every entry below it — see [`parse_db_header`].
///
/// A db packed with gzip, bzip2 or Unix compress (`csdb.txt.gz`) is unpacked
/// first, so it can be loaded exactly like the plain text file.
///
/// `filter` narrows down which lines are collected — see [`DbFilter`].
pub fn collect_db(path: &Path, filter: &DbFilter, out: &mut Vec<EmuFile>) -> Result<()> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(err) => bail!("Failed to read db file {}: {err}", path.display()),
    };
    let text = db_text(data, &format!("db file {}", path.display()))?;
    collect_db_text(&text, filter, out);
    Ok(())
}

/// Turn the raw bytes of a db into its text, unpacking it first when it is a
/// packed file (see [`unpack_if_packed`]). `what` names the source for errors.
fn db_text(data: Vec<u8>, what: &str) -> Result<String> {
    let data = match unpack_if_packed(data) {
        Ok(data) => data,
        Err(err) => bail!("Failed to unpack {what}: {err}"),
    };
    match String::from_utf8(data) {
        Ok(text) => Ok(text),
        Err(err) => bail!("Failed to read {what}: {err}"),
    }
}

/// Which db lines to keep, as regexes matched against the raw line before it is
/// parsed — the same thing piping the db through `grep`/`grep -v` would do, so
/// a pattern can pick on any field (`category:Demo`, `author:Fairlight`).
///
/// A line has to match *every* `include` pattern and must not match *any*
/// `exclude` pattern, so repeating `-I` narrows the selection down while
/// repeating `-X` widens what is thrown away. Header comments are always read,
/// so the platform and tags they set still apply to the lines that survive.
#[derive(Default)]
pub struct DbFilter<'a> {
    pub include: &'a [Regex],
    pub exclude: &'a [Regex],
}

impl DbFilter<'_> {
    fn keeps(&self, line: &str) -> bool {
        self.include.iter().all(|re| re.is_match(line))
            && !self.exclude.iter().any(|re| re.is_match(line))
    }
}

/// Load a db piped in on stdin, so entries can be filtered before they reach
/// demarc (`grep Amiga bitworld.txt | demarc`). Anything on stdin is taken to
/// be a db in the format [`collect_db`] describes, packed (`demarc < csdb.txt.gz`)
/// or not.
///
/// Does nothing when stdin is a terminal — there's nothing piped in then, and
/// reading would just block waiting for the user to type a db.
pub fn collect_db_stdin(filter: &DbFilter, out: &mut Vec<EmuFile>) -> Result<()> {
    if io::stdin().is_terminal() {
        return Ok(());
    }
    let mut data = Vec::new();
    if let Err(err) = io::stdin().read_to_end(&mut data) {
        bail!("Failed to read db from stdin: {err}");
    }
    let text = db_text(data, "db from stdin")?;
    collect_db_text(&text, filter, out);
    Ok(())
}

/// Read a header comment such as `# Platform:Amiga puae_model:A500`, which
/// applies to every line below it: `Platform` names the platform prefixed to
/// the type, and every other pair becomes a tag merged into each entry (a
/// db can this way set emulator settings for all its lines at once).
///
/// Only a comment made up entirely of `key:value` pairs is a header, so an
/// ordinary prose comment never turns into tags.
fn parse_db_header<'a>(
    comment: &'a str,
    platform: &mut Option<&'a str>,
    tags: &mut HashMap<String, String>,
) {
    let Some(fields) = comment
        .split_whitespace()
        .map(|field| field.split_once(':'))
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    if fields.is_empty() || fields.iter().any(|(k, v)| k.is_empty() || v.is_empty()) {
        return;
    }
    for (key, val) in fields {
        if key.eq_ignore_ascii_case("platform") {
            *platform = Some(val);
        } else {
            tags.insert(key.to_string(), val.to_string());
        }
    }
}

/// Parse the contents of a db file — see [`collect_db`] for the format.
pub(crate) fn collect_db_text(text: &str, filter: &DbFilter, out: &mut Vec<EmuFile>) {
    let mut file_platform: Option<&str> = None;

    // Tags from header comments, applied to every entry below them.
    let mut file_tags = HashMap::<String, String>::new();

    for l in text.lines() {
        let line = l.trim();

        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            parse_db_header(comment, &mut file_platform, &mut file_tags);
            continue;
        }
        if !filter.keeps(line) {
            continue;
        }
        let rec = parse_named_db_line(line).unwrap_or_else(|| parse_positional_db_line(line));

        let demo_type = match rec.platform.or(file_platform) {
            Some(platform) if !platform.is_empty() => format!("{platform} {}", rec.demo_type),
            _ => rec.demo_type.to_string(),
        };
        let url = rec.url.trim();
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

        let year = rec
            .date
            .split(['-', '/', '.'])
            .next()
            .unwrap_or_default()
            .to_string();
        // Header tags first, so anything the line itself names wins.
        let mut tags = file_tags.clone();
        for (key, val) in [
            ("party", rec.party),
            ("type", demo_type.as_str()),
            ("tags", rec.tag_list),
        ] {
            if !val.is_empty() {
                tags.insert(key.to_string(), val.to_string());
            }
        }

        out.push(EmuFile {
            path: FileSource::Url(urls),
            system_type: SystemType::Unknown,
            tags,
            game_info: GameInfo {
                title: rec.title.to_string(),
                group: rec.group.to_string(),
                year,
                typ: demo_type,
            },
        });
    }
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
            // NOTE: Images on disk are assumed to be screenshots
            if t != SystemType::Unknown && t != SystemType::Gfx {
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
    // Amiga and Atari ST: Always add parent dir (to get data files) — both
    // cores can mount the whole directory as a hard drive.
    let whole_dir = matches!(found_type, SystemType::Amiga | SystemType::AtariST);
    if mixed || ((!disk_images) && !whole_dir) {
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
fn stage_for_convert(path: &Path, already_temp: bool) -> Result<Option<TempDir>> {
    if already_temp || !has_convertible(path) {
        return Ok(None);
    }
    let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
    if path.is_dir() {
        copy_dir_all(path, dir.path())?;
    } else {
        fs::copy(path, dir.path().join(path.file_name().unwrap()))?;
    }
    debug!("FMT: staged {path:?} for conversion in {:?}", dir.path());
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

/// The extensions TOS starts a program under. A release names what the user is
/// meant to run this way and leaves its payload something else (`.BIN`, `.DAT`),
/// even when that payload is a GEMDOS executable in its own right.
const ATARI_PROGRAM_EXTS: [&str; 4] = ["prg", "tos", "ttp", "app"];

/// True if `path` is named the way a program meant to be started is.
fn has_atari_program_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ATARI_PROGRAM_EXTS.iter().any(|w| e.eq_ignore_ascii_case(w)))
}

/// The Atari program a release directory boots, searching the top level before
/// descending — a demo sitting next to its data files wins over anything buried
/// deeper.
///
/// Extension decides first: a `.tos`/`.prg`/`.ttp` beats an executable named
/// anything else, however small. A multi-part demo is a tiny loader next to huge
/// `.bin` parts it Pexecs after reading their data files (More Or Less Zero:
/// `molz.tos` is 651 bytes, `part1.bin` 652K) — start the part directly and it
/// runs on data that was never loaded.
///
/// Within a level the biggest program wins: a release ships the demo alongside a
/// readme viewer, an installer or the disk-swap stubs of its floppy version, and
/// the demo itself dwarfs all of them. Name breaks a tie, so the pick is stable.
/// Name the file with a `boot_file` tag to decide it outright.
fn find_atari_program(dir: &Path) -> Option<PathBuf> {
    let mut files = vec![];
    let mut dirs = vec![];
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() { &mut dirs } else { &mut files }.push(path);
    }
    let mut programs: Vec<(bool, u64, PathBuf)> = files
        .into_iter()
        .filter(|f| is_atari_program(f))
        .map(|f| {
            let len = fs::metadata(&f).map_or(0, |m| m.len());
            (has_atari_program_ext(&f), len, f)
        })
        .collect();
    programs.sort_by(|(a_ext, a_len, a), (b_ext, b_len, b)| {
        b_ext
            .cmp(a_ext)
            .then_with(|| b_len.cmp(a_len))
            .then_with(|| a.cmp(b))
    });
    if let Some((_, _, prg)) = programs.into_iter().next() {
        return Some(prg);
    }
    dirs.sort();
    dirs.iter().find_map(|d| find_atari_program(d))
}

/// Compare a file name the way the Atari and Amiga file systems a release comes
/// off would: case doesn't matter, and neither does which slash separates the
/// path (`AUTO\DEMO.PRG` is `auto/demo.prg`).
fn name_key(name: &str) -> String {
    name.replace('\\', "/")
        .trim_matches('/')
        .to_ascii_lowercase()
}

/// Find the file a `boot_file` tag names under `dir`, matching either its bare
/// name (`TLKTLK2.PRG`) or its path relative to the release
/// (`TALKTALK.2/TLKTLK2.PRG`). Shallower matches win, so the name alone is
/// usually enough.
fn find_boot_file(dir: &Path, base: &Path, wanted: &str) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries.iter().filter(|p| !p.is_dir()) {
        let rel = path.strip_prefix(base).unwrap_or(path);
        let name = path.file_name().unwrap_or_default();
        if name_key(&rel.to_string_lossy()) == wanted || name_key(&name.to_string_lossy()) == wanted
        {
            return Some(path.clone());
        }
    }
    entries
        .iter()
        .filter(|p| p.is_dir())
        .find_map(|d| find_boot_file(d, base, wanted))
}

/// The file a `boot_file` tag names, looked up in the release directory. An
/// unresolvable name is warned about rather than failed on — the release still
/// loads the way it would without the tag.
fn tagged_boot_file(dir: &Path, tags: &HashMap<String, String>) -> Option<PathBuf> {
    let name = tags.get("boot_file")?;
    let found = find_boot_file(dir, dir, &name_key(name));
    if found.is_none() {
        warn!("boot_file {name:?} not found in {dir:?}");
    }
    found
}

/// The Atari program `dir` should be booted from as a GEMDOS hard drive, if
/// there is one. A directory that boils down to the program alone is left to
/// [`build_atari_auto_disk`] instead: a lone executable boots from a floppy the
/// way it was released, and that is what old demos expect.
fn atari_hard_drive(dir: &Path) -> Option<PathBuf> {
    if only_file(dir).is_some() {
        return None;
    }
    find_atari_program(dir)
}

/// Name given to the program copied into the drive's `AUTO` folder. TOS only
/// auto-starts `.PRG` files from there, so a `.TOS` or `.TTP` demo has to be
/// renamed to run — and 8.3 keeps GEMDOS from mangling the name.
const AUTO_PROGRAM: &str = "STARTME.PRG";

/// Where a release's own `AUTO` folder is moved when we start the program
/// ourselves. Its contents are kept, just not under the one name TOS runs on
/// boot.
const DISABLED_AUTO: &str = "NOAUTO";

/// `dir/name`, numbered (`NOAUTO2`, `NOAUTO3`, ...) until nothing of that name
/// is there already.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let mut path = dir.join(name);
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{name}{n}"));
    }
    path
}

/// Stage `prg` and everything next to it as an Atari ST hard drive for hatari,
/// which mounts a host directory as GEMDOS drive C: and boots from it. Returns
/// the path to hand the core and the temp directory holding it, which the
/// caller has to keep alive for as long as the drive is needed.
///
/// The libretro core takes the drive as a `.gem` file whose name, minus that
/// extension, is the directory to mount — so the two are created side by side,
/// the `.gem` itself empty.
///
/// Booting from the drive runs `C:\AUTO\*.PRG`, so that is where `prg` goes. A
/// release that keeps its own program in `AUTO` already starts itself and is
/// left alone; any *other* `AUTO` folder is moved aside first, because what a
/// hard drive release carries there is usually the disk-swap stubs of its floppy
/// version, which stop the boot dead ("insert disk 1 and reboot").
///
/// Everything is copied into the temp directory rather than mounted in place:
/// the drive is writable from the emulator, and the `AUTO` folder is ours to
/// rearrange — neither is something to do to a directory of the user's own.
fn build_gemdos_drive(prg: &Path) -> Result<(PathBuf, TempDir)> {
    let mut root = prg.parent().unwrap_or(Path::new("."));
    // A program already in an `AUTO` folder is started by the folder above it.
    let in_auto = root
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("auto"));
    if in_auto && let Some(parent) = root.parent() {
        root = parent;
    }

    let temp = tempfile::Builder::new().prefix("demarc-").tempdir()?;
    let drive = temp.path().join("harddrive");
    copy_dir_all(root, &drive)?;

    if !in_auto {
        if let Some(auto) = find_child(&drive, "auto").filter(|a| a.is_dir()) {
            let aside = free_name(&drive, DISABLED_AUTO);
            debug!("FMT: moving the release's AUTO folder aside to {aside:?}");
            fs::rename(&auto, &aside)?;
        }
        let auto = drive.join("AUTO");
        fs::create_dir_all(&auto)?;
        fs::copy(prg, auto.join(AUTO_PROGRAM))?;
    }

    let gem = temp.path().join("harddrive.gem");
    fs::write(&gem, [])?;
    Ok((gem, temp))
}

/// Prepare a file for loading into emulator. Unpack archives, parse
/// binaries for more info, convert formats as needed.
/// Also need to determine system_type if not done previously
pub fn prepare_file(emu_file: &EmuFile, in_tags: &HashMap<String, String>) -> Result<WorkingFile> {
    let EmuFile {
        mut system_type,
        path: mut source,
        mut tags,
        game_info,
    } = emu_file.clone();
    // Holds whichever temp directory `path` currently points into, so it lives
    // exactly as long as the `WorkingFile` this returns. Each stage below that
    // builds a new one first copies out what it needs from the old, so simply
    // replacing this drops — and removes — the directory it is done with.
    let mut temp_dir: Option<TempDir> = None;

    for (key, val) in in_tags {
        tags.insert(key.clone(), val.clone());
    }

    // Entries backed by a URL (e.g. from a `--db` list) are downloaded to the
    // local cache on first load, then handled like any other local file. The
    // cache means later loads of the same URL are free. Re-detect the system
    // type once we have the real file, since collection only saw the URL.
    let was_url = matches!(source, FileSource::Url(_));
    let mut path = source.resolve()?.clone();
    if was_url {
        system_type = get_system_type(&path);
    }

    if path.is_file()
        && let Some(unpacked) = unpack_to_temp(&path)?
    {
        debug!("FMT: unpacked archive {path:?} -> {:?}", unpacked.path());
        path = unpacked.path().to_path_buf();
        temp_dir = Some(unpacked);
        system_type = get_system_type(&path);
    }

    // A file that needs converting becomes a temp directory holding just that
    // file, so from here on there is one route: convert the directory, then
    // pick the file back out of it below.
    if let Some(staged) = stage_for_convert(&path, temp_dir.is_some())? {
        path = staged.path().to_path_buf();
        temp_dir = Some(staged);
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
            // A `boot_file` tag names what to start and overrides the scan,
            // wherever in the release the file sits.
            let boot = tagged_boot_file(&path, &tags);
            let program = match &boot {
                Some(f) => is_atari_program(f).then(|| f.clone()),
                None if files.is_empty() => atari_hard_drive(&path),
                None => None,
            };
            if let Some(prg) = program {
                debug!("FMT: Atari GEMDOS hard drive booting {prg:?}");
                let (gem, dir) = build_gemdos_drive(&prg)?;
                path = gem;
                temp_dir = Some(dir);
                system_type = SystemType::AtariST;
                // A release shipped on a hard drive is from the late ST era and
                // outgrew the floppy machine: nothing that needs a hard drive
                // ran on a 1 MB ST. `--ste`/`--xmem` and any tag still win.
                for (key, val) in [("hatari_machinetype", "ste"), ("hatari_ramsize", "4")] {
                    tags.entry(key.into()).or_insert_with(|| val.into());
                }
            } else if let Some(f) = boot {
                path = f;
                copy_all = true;
            } else if files.len() > 1 {
                sort_disks(&mut files);
                let (m3u, dir) = build_m3u(&files)?;
                path = m3u;
                temp_dir = Some(dir);
            } else if let Some(f) = scan.first_file.or_else(|| only_file(&path)) {
                path = f;
                copy_all = true;
            }
        }
    }

    if !path.is_dir() {
        if is_psx_exe(&path) {
            // Only Beetle loads a raw PS-X EXE, and only with a real BIOS. Wrap
            // the executable in a bootable disc image instead and the default
            // core takes it, HLE BIOS and all. The image lives in the cache and
            // is reused between runs, so it deliberately isn't the `temp_dir`.

            if tags.get("psx_core").is_none() {
                debug!("Building iso");
                let disc = create_psx_iso(&path).unwrap_or_else(|err| {
                    warn!("Could not build a disc image for {path:?}: {err}");
                    None
                });
                if let Some(disc) = disc {
                    debug!("FMT: PSX executable wrapped in disc image {disc:?}");
                    path = disc;
                } else {
                    tags.insert("psx_core".into(), "beetle".into());
                    // A scene exe whose header undercounts its text section doesn't
                    // load at all, so patch the header before the core sees it. The
                    // file we were handed isn't ours to write to unless it already
                    // came out of a temp directory, so anything else is copied first.
                    if psx_needs_text_fix(&path) {
                        if temp_dir.is_none() {
                            // TODO: temp file leek? Better to wire this in convert_dir()
                            let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
                            let copy = dir.path().join(path.file_name().unwrap());
                            fs::copy(&path, &copy)?;
                            path = copy;
                            temp_dir = Some(dir);
                        }
                        debug!("FMT: patching short PSX text section in {path:?}");
                        fix_psx_text_size(&path)?;
                    }
                }
            }
        }

        // Only the header is examined below — the GBA check reaches furthest
        // into it; the Amiga branch works off `path`, not the contents.
        let data = read_header(&path, GBA_HEADER_LEN)?;
        if data.starts_with(&GEMDOS_MAGIC) {
            // GEMDOS executable: wrap it in a bootable Atari ST floppy image
            // with the program in the AUTO folder so it runs on boot. The whole
            // executable goes on the disk, not just the header read above.
            let (img, dir) = build_atari_auto_disk(&fs::read(&path)?)?;
            path = img;
            temp_dir = Some(dir);
            system_type = SystemType::AtariST;
        } else if data.len() >= 2 && data[0..2] == [0x01, 0x08] {
            system_type = SystemType::C64;
        } else if is_gba_rom(&data) {
            debug!("FMT: GBA rom: {path:?}");
            system_type = SystemType::Gba;
        } else if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
            debug!("FMT: Amiga exe: {path:?}");
            if std::fs::metadata(&path)?.len() > 850 * 1024 {
                tags.insert("puae_model".into(), "A1200".into());
            }
            let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
            let target_dir = dir.path().to_path_buf();
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
            temp_dir = Some(dir);
            tags.insert("puae_use_whdload".into(), "disabled".into());
            system_type = SystemType::Amiga;
        }
    };

    // The rewritten disc lives in the cache and is reused across runs, so it
    // deliberately doesn't become the `temp_dir` — that would delete it. Any
    // temp directory the cue itself came out of stays held either way.
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
        temp_dir,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The local path behind a source. Everything below collects real files, so
    /// a URL here means the test itself is wrong.
    fn local_path(source: &FileSource) -> &Path {
        match source {
            FileSource::Path(p) => p,
            FileSource::Url(urls) => panic!("expected a local path, got {urls:?}"),
        }
    }

    #[test]
    fn load_demo() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = vec![];
        collect_files(&root.join("demos/nexus7"), &mut out, false).unwrap();
        println!("{:?}", out[0]);
        let wf = prepare_file(&out[0], &HashMap::new());
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
                let path = local_path(&f.path);
                let rel = path.strip_prefix(root).unwrap_or(path);
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
                ("demos/o2-intro".into(), SystemType::Unknown),
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
        assert_eq!(type_of(&found, "testdata/fr018.bin"), SystemType::Gba);

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
        prepare_file(&collect_file(&root.join(rel)).unwrap(), &HashMap::new()).unwrap()
    }

    /// A bare GEMDOS executable is wrapped in a bootable floppy image, so the
    /// prepared path is no longer the `.prg` handed in.
    #[test]
    fn atari_exe() {
        let wf = prepare("demos/natrium.prg");
        assert_eq!(wf.system_type, SystemType::AtariST);
        assert_eq!(wf.path.extension().unwrap(), "st");
        assert!(wf.temp_dir.is_some());
    }

    /// A release directory holding an ST program next to its data is mounted as
    /// a GEMDOS hard drive: the whole tree is copied to the drive and the
    /// program is copied into `AUTO` so TOS starts it when it boots from C:.
    #[test]
    fn atari_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            root.join("demos/natrium.prg"),
            dir.path().join("NATRIUM.PRG"),
        )
        .unwrap();
        fs::create_dir(dir.path().join("DATA")).unwrap();
        fs::write(dir.path().join("DATA/MUSIC.SND"), b"data").unwrap();

        let wf = prepare_file(
            &EmuFile {
                path: dir.path().into(),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(wf.system_type, SystemType::AtariST);
        assert_eq!(wf.path.extension().unwrap(), "gem");
        // The core mounts the directory named by the `.gem` file minus that
        // extension, so the two have to sit side by side.
        let drive = wf.path.with_extension("");
        assert!(drive.join("AUTO/STARTME.PRG").is_file());
        assert!(drive.join("NATRIUM.PRG").is_file());
        assert!(drive.join("DATA/MUSIC.SND").is_file());
        // The user's own directory is left exactly as it was.
        assert!(!dir.path().join("AUTO").exists());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    /// Build a release directory: `files` are `(relative path, size)`, each
    /// written as a GEMDOS program of that size, and every other entry as data.
    /// A name is a program when it is named like one (`.prg`, `.tos`, ...) or is
    /// marked with a leading `*`, which a release uses for payloads it Pexecs
    /// under a data-looking name (`*PART1.BIN`). The marker isn't part of the
    /// file name.
    fn release_dir(files: &[(&str, usize)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, size) in files {
            let forced = name.strip_prefix('*');
            let name = forced.unwrap_or(name);
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut data = if forced.is_some() || has_atari_program_ext(Path::new(name)) {
                GEMDOS_MAGIC.to_vec()
            } else {
                b"da".to_vec()
            };
            data.resize(*size, 0);
            fs::write(&path, data).unwrap();
        }
        dir
    }

    /// Prepare a directory the way the frontend does, with `tags` applied.
    fn prepare_dir(dir: &Path, tags: &[(&str, &str)]) -> WorkingFile {
        let tags = tags
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        prepare_file(
            &EmuFile {
                path: dir.into(),
                ..Default::default()
            },
            &tags,
        )
        .unwrap()
    }

    /// The demo dwarfs the readme viewer and installer a release ships next to
    /// it, so size — not alphabetical order — decides what is started.
    #[test]
    fn atari_biggest_program_wins() {
        let dir = release_dir(&[
            ("DEMO.PRG", 120_000),
            ("AREADME.PRG", 40_000),
            ("INSTALL.PRG", 8_000),
            ("DATA/MUSIC.SND", 4),
        ]);
        assert_eq!(
            find_atari_program(dir.path()).unwrap(),
            dir.path().join("DEMO.PRG")
        );
    }

    /// A multi-part demo is a tiny loader that reads the data files and Pexecs
    /// its parts, which are GEMDOS executables too — and far bigger. Extension
    /// beats size, so the loader is what starts.
    #[test]
    fn atari_program_extension_beats_size() {
        let dir = release_dir(&[
            ("MOLZ.TOS", 651),
            ("*PART1.BIN", 667_822),
            ("*PART2.BIN", 205_821),
            ("PART1.MUS", 618_487),
        ]);
        assert_eq!(
            find_atari_program(dir.path()).unwrap(),
            dir.path().join("MOLZ.TOS")
        );
    }

    /// A hard drive release carries the `AUTO` folder of its floppy version,
    /// full of disk-swap stubs that stop the boot. Since the program we start
    /// isn't one of them, the folder is moved aside — kept, but no longer the
    /// name TOS runs on boot — and our own starter takes its place.
    #[test]
    fn atari_dir_leftover_auto_moved_aside() {
        let dir = release_dir(&[
            ("TLKTLK2.PRG", 120_000),
            ("TLK2READ.PRG", 40_000),
            ("AUTO/DISK2.PRG", 3_500),
            ("AUTO/LOADER.PRG", 5_000),
            ("TLKTLK2.D8A/TLK2_100.D8A", 1_766),
        ]);

        let wf = prepare_dir(dir.path(), &[]);
        assert_eq!(wf.system_type, SystemType::AtariST);
        let drive = wf.path.with_extension("");
        assert!(drive.join("AUTO/STARTME.PRG").is_file());
        assert!(
            !drive.join("AUTO/DISK2.PRG").exists(),
            "stub still auto-runs"
        );
        // Nothing is lost, it just isn't called AUTO any more.
        assert!(drive.join("NOAUTO/DISK2.PRG").is_file());
        assert!(drive.join("NOAUTO/LOADER.PRG").is_file());
        // The biggest program is the one that got copied in.
        assert_eq!(
            fs::metadata(drive.join("AUTO/STARTME.PRG")).unwrap().len(),
            120_000
        );
        // A hard drive release gets the late-era machine it was made for.
        assert_eq!(wf.settings.get("hatari_machinetype").unwrap(), "ste");
        assert_eq!(wf.settings.get("hatari_ramsize").unwrap(), "4");
    }

    /// `--ste`/`--xmem` and any explicit tag win over the hard drive defaults.
    #[test]
    fn atari_dir_machine_tags_win() {
        let dir = release_dir(&[("DEMO.PRG", 120_000), ("DATA.BIN", 4)]);
        let wf = prepare_dir(
            dir.path(),
            &[("hatari_machinetype", "st"), ("hatari_ramsize", "8")],
        );
        assert_eq!(wf.settings.get("hatari_machinetype").unwrap(), "st");
        assert_eq!(wf.settings.get("hatari_ramsize").unwrap(), "8");
    }

    /// `boot_file` names the program to start, whatever the size heuristic
    /// would have picked. The name is matched the way the Atari file system
    /// would: case doesn't matter, and a path may be given to disambiguate.
    #[test]
    fn atari_boot_file_tag() {
        let files = [
            ("DEMO.PRG", 120_000),
            ("MENU.PRG", 40_000),
            ("EXTRA/BONUS.PRG", 20_000),
        ];
        for name in ["MENU.PRG", "menu.prg", "EXTRA\\BONUS.PRG"] {
            let dir = release_dir(&files);
            let wf = prepare_dir(dir.path(), &[("boot_file", name)]);
            let started = fs::metadata(wf.path.with_extension("").join("AUTO/STARTME.PRG"))
                .unwrap()
                .len();
            let expected = if name.to_ascii_lowercase().contains("bonus") {
                20_000
            } else {
                40_000
            };
            assert_eq!(started, expected, "boot_file {name:?}");
        }
    }

    /// A `boot_file` naming something that isn't there is a warning, not a
    /// failure: the release loads the way it would without the tag.
    #[test]
    fn atari_boot_file_missing() {
        let dir = release_dir(&[("DEMO.PRG", 120_000), ("DATA.BIN", 4)]);
        let wf = prepare_dir(dir.path(), &[("boot_file", "NOSUCH.PRG")]);
        assert_eq!(wf.system_type, SystemType::AtariST);
        assert_eq!(
            fs::metadata(wf.path.with_extension("").join("AUTO/STARTME.PRG"))
                .unwrap()
                .len(),
            120_000
        );
    }

    /// A release that brings its own `AUTO` folder starts what is already in
    /// there, and the drive is the directory holding that folder — not the
    /// folder itself.
    #[test]
    fn atari_dir_with_auto_folder() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("auto")).unwrap();
        fs::copy(
            root.join("demos/natrium.prg"),
            dir.path().join("auto/DEMO.PRG"),
        )
        .unwrap();
        fs::write(dir.path().join("MUSIC.SND"), b"data").unwrap();

        let wf = prepare_file(
            &EmuFile {
                path: dir.path().into(),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(wf.system_type, SystemType::AtariST);
        let drive = wf.path.with_extension("");
        assert!(drive.join("MUSIC.SND").is_file());
        assert!(drive.join("auto/DEMO.PRG").is_file());
        assert!(!drive.join("auto/STARTME.PRG").exists());
    }

    /// A directory holding nothing but the program keeps taking the floppy
    /// route — a bootable disk is what a lone ST executable was released as.
    #[test]
    fn atari_single_program_dir() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let dir = tempfile::tempdir().unwrap();
        fs::copy(
            root.join("demos/natrium.prg"),
            dir.path().join("NATRIUM.PRG"),
        )
        .unwrap();

        let wf = prepare_file(
            &EmuFile {
                path: dir.path().into(),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(wf.system_type, SystemType::AtariST);
        assert_eq!(wf.path.extension().unwrap(), "st");
    }

    /// An ST release directory is collected as the directory itself, the way an
    /// Amiga one is, so the data files next to the program come along.
    #[test]
    fn collects_atari_dir_whole() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let release = dir.path().join("release");
        fs::create_dir(&release).unwrap();
        fs::copy(root.join("demos/natrium.prg"), release.join("NATRIUM.PRG")).unwrap();
        fs::copy(root.join("demos/natrium.prg"), release.join("PART2.PRG")).unwrap();

        let mut out = vec![];
        collect_files(dir.path(), &mut out, false).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(local_path(&out[0].path), release);

        // `many` splits the same directory back into its programs.
        let mut out = vec![];
        collect_files(dir.path(), &mut out, true).unwrap();
        assert_eq!(out.len(), 2);
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
        assert!(wf.temp_dir.is_some());
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
        assert!(wf.temp_dir.is_some());
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

        let wf = prepare_file(
            &EmuFile {
                path: dir.path().into(),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path.extension().unwrap(), "prg");
        assert!(!wf.path.starts_with(dir.path()));
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    /// A PSX executable is wrapped in a bootable disc image on the way to the
    /// core, so the default core — which loads discs, not executables — can take
    /// it without a BIOS. The short text section it came with is repaired inside
    /// that image; the file we were handed isn't ours to write to.
    #[test]
    fn psx_exe_is_wrapped_in_a_disc() {
        let dir = tempfile::tempdir().unwrap();
        // The cached image is named after the executable, so a stem no other
        // test uses keeps the two from tidying up each other's cache entry.
        let exe = dir.path().join("wrapped-demo.psx");
        let mut data = b"PS-X EXE".to_vec();
        data.resize(0x18, 0);
        data.extend_from_slice(&0x8001_0000u32.to_le_bytes()); // t_addr
        data.extend_from_slice(&0x800u32.to_le_bytes()); // t_size, way short
        data.resize(0x800 + 0x4000, 0);
        fs::write(&exe, &data).unwrap();

        let wf = prepare_file(
            &EmuFile {
                path: exe.as_path().into(),
                ..Default::default()
            },
            &HashMap::new(),
        )
        .unwrap();

        assert_eq!(wf.path.extension().unwrap(), "iso");
        assert_eq!(wf.settings.get("psx_core"), None, "no Beetle, no BIOS");

        let image = fs::read(&wf.path).unwrap();
        // The executable is on the disc with the size its header undercounted.
        let on_disc = image.windows(8).position(|w| w == b"PS-X EXE").unwrap();
        assert_eq!(
            u32::from_le_bytes(image[on_disc + 0x1c..on_disc + 0x20].try_into().unwrap()),
            0x4000
        );
        let t_size = |p: &Path| {
            let header = read_header(p, 0x20).unwrap();
            u32::from_le_bytes(header[0x1c..0x20].try_into().unwrap())
        };
        assert_eq!(t_size(&exe), 0x800, "the original is left alone");
        let _ = fs::remove_file(&wf.path);
    }

    /// A GBA rom is recognised by its header, not its name — this one is a
    /// `.bin` — and is loaded as-is.
    #[test]
    fn gba_rom() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let wf = prepare("testdata/fr018.bin");
        assert_eq!(wf.system_type, SystemType::Gba);
        assert_eq!(wf.path, root.join("testdata/fr018.bin"));
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
        collect_db(&db, &DbFilter::default(), &mut out).unwrap();
        assert_eq!(out.len(), 2, "blank and URL-less lines skipped");

        let eod = &out[0];
        let FileSource::Url(urls) = &eod.path else {
            panic!("db entries stay URLs until loaded, got {:?}", eod.path)
        };
        assert_eq!(
            urls.iter().map(Url::as_str).collect::<Vec<_>>(),
            ["https://example.com/eod.d64"]
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
        //println!("{:?}", nexus.tags);
        //assert!(nexus.tags.is_empty(), "empty scene fields left out");
    }

    /// The named format spells out each field, so order doesn't matter and a
    /// value may hold `:` itself (every URL does). Field names line up with the
    /// same metadata the positional format carries.
    #[test]
    fn collect_db_parses_named_fields() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demos.txt");
        fs::write(
            &db,
            "id:1\ttitle:Zentro 4\tauthor:Zenith\tdate:1992-12-27\tparty:The Party 1992\tcategory:Demo\ttags:has effects\tdownload:http://example.com/zentro4;http://example.com/zentro4.dms\n\
             \n\
             download:https://example.com/nexus7.zip\tauthor:Andromeda\ttitle:Nexus 7\tdate:1994/12/30\n\
             id:3\ttitle:No URL\tauthor:Group\tdate:1994\tparty:\tcategory:Intro\ttags:\tdownload:\n\
             id:4\ttitle:Musicdisk\tauthor:Group\tdate:1992\tparty:\tcategory:Musicdisk\ttags:\tdownload:http://example.com/md.dms\n",
        )
        .unwrap();

        let mut out = vec![];
        collect_db(&db, &DbFilter::default(), &mut out).unwrap();
        assert_eq!(out.len(), 3, "blank, URL-less and disk lines skipped");

        let zentro = &out[0];
        let FileSource::Url(urls) = &zentro.path else {
            panic!("db entries stay URLs until loaded, got {:?}", zentro.path)
        };
        assert_eq!(
            urls.iter().map(Url::as_str).collect::<Vec<_>>(),
            [
                "http://example.com/zentro4",
                "http://example.com/zentro4.dms"
            ]
        );
        assert_eq!(zentro.game_info.title, "Zentro 4");
        assert_eq!(zentro.game_info.group, "Zenith");
        assert_eq!(zentro.game_info.year, "1992");
        assert_eq!(zentro.game_info.typ, "Demo");
        assert_eq!(zentro.tags.get("party").unwrap(), "The Party 1992");
        assert_eq!(zentro.tags.get("type").unwrap(), "Demo");
        assert_eq!(zentro.tags.get("tags").unwrap(), "has effects");

        // Fields in any order, missing ones simply left empty.
        let nexus = &out[1];
        assert_eq!(nexus.game_info.title, "Nexus 7");
        assert_eq!(nexus.game_info.group, "Andromeda");
        assert_eq!(nexus.game_info.year, "1994");
        assert!(!nexus.tags.contains_key("party"));
    }

    /// A packed db loads exactly like the plain text one it was made from — the
    /// dbs are big and ship compressed, so both gzip and bzip2 are unpacked on
    /// the way in.
    #[test]
    fn collect_db_reads_packed_dbs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for packed in ["testdata/demos.txt.gz", "testdata/demos.txt.bz2"] {
            let mut out = vec![];
            collect_db(&root.join(packed), &DbFilter::default(), &mut out).unwrap();
            assert_eq!(out.len(), 2, "{packed}");

            let eod = &out[0];
            assert_eq!(eod.game_info.title, "Edge of Disgrace", "{packed}");
            assert_eq!(eod.game_info.group, "Booze Design", "{packed}");
            assert_eq!(eod.game_info.year, "2008", "{packed}");
            // The `# Platform:C64` header applies just as it does unpacked.
            assert_eq!(eod.game_info.typ, "C64 demo", "{packed}");
            let FileSource::Url(urls) = &eod.path else {
                panic!("db entries stay URLs until loaded, got {:?}", eod.path)
            };
            assert_eq!(
                urls.iter().map(Url::as_str).collect::<Vec<_>>(),
                ["https://example.com/eod.d64"],
                "{packed}"
            );
            assert_eq!(out[1].game_info.title, "Nexus 7", "{packed}");
        }
    }

    /// A db piped in has usually been filtered line by line, so the header that
    /// carried the platform may be gone and only some lines survive — each line
    /// still stands on its own.
    #[test]
    fn collect_db_parses_filtered_lines() {
        let mut out = vec![];
        collect_db_text(
            "id:9\ttitle:Speedball Demo\tauthor:Illusions\tdate:1990-04-07\tcategory:Demo\tdownload:http://example.com/speedball\n",
            &DbFilter::default(),
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].game_info.title, "Speedball Demo");
        assert_eq!(out[0].game_info.typ, "Demo");
    }

    /// `--include`/`--exclude` drop non-matching lines while collecting, but
    /// header comments are still read so the platform they set reaches the
    /// survivors.
    #[test]
    fn collect_db_applies_filter() {
        const DB: &str = "# Platform:Amiga\n\
             id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
             id:2\ttitle:Musicdisk\tcategory:Musicdisk\tdownload:http://example.com/md.dms\n\
             id:3\ttitle:Nexus 7\tcategory:Demo\ttags:aga\tdownload:http://example.com/nexus7.zip\n";

        let titles = |filter: &DbFilter| {
            let mut out = vec![];
            collect_db_text(DB, filter, &mut out);
            (
                out.iter()
                    .map(|f| f.game_info.title.clone())
                    .collect::<Vec<_>>(),
                out,
            )
        };

        let re = |p: &str| [Regex::new(p).unwrap()];

        let include = re("category:Demo");
        let (kept, out) = titles(&DbFilter {
            include: &include,
            ..Default::default()
        });
        assert_eq!(kept, ["Zentro 4", "Nexus 7"]);
        assert_eq!(out[0].game_info.typ, "Amiga Demo", "header still applies");

        let exclude = re("category:Musicdisk");
        let (kept, _) = titles(&DbFilter {
            exclude: &exclude,
            ..Default::default()
        });
        assert_eq!(kept, ["Zentro 4", "Nexus 7"]);

        // Both apply: a line has to match `include` and miss `exclude`.
        let exclude = re("tags:aga");
        let (kept, _) = titles(&DbFilter {
            include: &include,
            exclude: &exclude,
        });
        assert_eq!(kept, ["Zentro 4"]);

        // Several includes are AND:ed — only the line matching both survives.
        let include = [
            Regex::new("category:Demo").unwrap(),
            Regex::new("aga").unwrap(),
        ];
        let (kept, _) = titles(&DbFilter {
            include: &include,
            ..Default::default()
        });
        assert_eq!(kept, ["Nexus 7"]);

        // Several excludes are OR:ed — matching either one drops the line.
        let exclude = [
            Regex::new("category:Musicdisk").unwrap(),
            Regex::new("tags:aga").unwrap(),
        ];
        let (kept, _) = titles(&DbFilter {
            exclude: &exclude,
            ..Default::default()
        });
        assert_eq!(kept, ["Zentro 4"]);
    }

    /// A `platform` field, or a `# Platform:` header covering the lines below
    /// it, prefixes the type so entries from different scenes stay apart.
    #[test]
    fn collect_db_prefixes_platform() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demos.txt");
        fs::write(
            &db,
            "# Platform:Amiga\n\
             id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
             id:2\ttitle:Embryo\tcategory:Demo\tplatform:C64\tdownload:http://example.com/embryo.zip\n",
        )
        .unwrap();

        let mut out = vec![];
        collect_db(&db, &DbFilter::default(), &mut out).unwrap();
        assert_eq!(out[0].game_info.typ, "Amiga Demo", "header applies");
        assert_eq!(out[1].game_info.typ, "C64 Demo", "line overrides header");
    }

    /// The other pairs of a header line become tags on every entry below it,
    /// while a plain prose comment is left alone.
    #[test]
    fn collect_db_applies_header_tags() {
        let mut out = vec![];
        collect_db_text(
            "# Platform:Amiga puae_model:A500\n\
             # Just a comment: nothing to see here\n\
             id:1\ttitle:Zentro 4\tcategory:Demo\tdownload:http://example.com/zentro4\n\
             # puae_model:A1200\n\
             id:2\ttitle:Nexus 7\tcategory:Demo\ttags:aga\tdownload:http://example.com/nexus7.zip\n",
            &DbFilter::default(),
            &mut out,
        );

        assert_eq!(out[0].game_info.typ, "Amiga Demo");
        assert_eq!(out[0].tags.get("puae_model").unwrap(), "A500");
        assert!(!out[0].tags.contains_key("Just"));
        assert!(!out[0].tags.contains_key("comment"));

        // A later header overrides, and the platform from the first one still
        // applies.
        assert_eq!(out[1].tags.get("puae_model").unwrap(), "A1200");
        assert_eq!(out[1].tags.get("tags").unwrap(), "aga");
        assert_eq!(out[1].game_info.typ, "Amiga Demo");
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
        out.sort_by(|a, b| local_path(&a.path).cmp(local_path(&b.path)));
        let wf = prepare_file(&out[0], &HashMap::new()).unwrap();
        assert_eq!(wf.system_type, SystemType::C64);
        assert_eq!(wf.path, local_path(&out[0].path));
    }
}

