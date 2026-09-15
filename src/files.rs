use std::{
    collections::HashMap,
    fs,
    io::{self, IsTerminal, Read},
    path::Path,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use tracing::info;

use crate::{
    emu_file::{CompactDate, EmuFile, FileSource, GameInfo, UrlList},
    m3u::M3u,
    utils::{is_disk_image, unpack_if_packed},
};

fn handle_m3u(in_path: &Path) -> Result<EmuFile> {
    let mut title: &'static str = "";
    let mut group: &'static str = "";
    let mut date = CompactDate::default();
    let mut meta = HashMap::new();

    let m3u = M3u::from_file(in_path)?;

    let path = (if m3u.files.is_empty() {
        in_path.parent().unwrap()
    } else {
        in_path
    })
    .to_owned();

    info!("{:?}", m3u.tags);
    for (key, val) in m3u.tags {
        if key == "title" {
            title = leak(val);
        } else if key == "group" {
            group = leak(val);
        } else if key == "year" {
            let year = val.parse::<u32>().unwrap_or(0);
            date = CompactDate::new(year, 0, 0);
        } else {
            meta.insert(leak(key), leak(val));
        }
    }
    Ok(EmuFile {
        path: path.into(),
        meta,
        game_info: GameInfo {
            title,
            group,
            date,
            category: "???",
            ..Default::default()
        },
    })
}

/// Give a runtime-built string the `'static` lifetime an [`EmuFile`] wants.
///
/// The file list is built once and kept for the whole run, so nothing collected
/// into it is ever freed anyway; leaking says so in the type and lets entries
/// hold plain `&'static str` instead of `String`. Only used for the handful of
/// strings that aren't already slices of the leaked db text — m3u tags and file
/// stems, and the overrides read at startup (see [`crate::overrides`]) — so the
/// leak is bounded by the size of the file list.
pub(crate) fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Parse a `key:value`-per-field line, e.g.
/// `id:1\ttitle:Zentro 4\tauthor:Zenith\t…`, into its pairs. Fields without a
/// `:` are skipped.
///
/// Only the first `:` splits a field, leaving values (URLs above all) intact,
/// and fields may appear in any order.
fn parse_named_db_line(line: &str) -> Vec<(&str, &str)> {
    let mut result = vec![];
    for field in line.split('\t') {
        let Some((key, val)) = field.split_once(':') else {
            continue;
        };
        result.push((key, val));
    }
    result
}

/// The pouet.net rank out of a db line's `pouet` field.
///
/// The field is `pouet:<cdc>,<thumbs>,<rank>,...` — the release's id on pouet,
/// how many thumbs up it has and where it sits in pouet's ranking, 1 being the
/// best. A release that isn't on pouet has no field at all, and one that is but
/// hasn't been ranked leaves the item empty, so both give `None`.
pub(crate) fn parse_pouet_rank(field: &str) -> Option<u32> {
    field.split(',').nth(2)?.trim().parse().ok()
}

/// Parse a tab-separated demo database file into `EmuFile` entries appended to
/// `out`.
///
/// Each non-blank line holds `key:value` fields separated by tabs, in any order
/// (`id:1\ttitle:Zentro 4\tauthor:Zenith\t…`). Every field becomes meta on the
/// entry; `title`, `author` and the year — the first `-`/`/`/`.`-delimited part
/// of `date` — additionally fill in its [`GameInfo`]. The `download` field
/// becomes the entry's path and is fetched on demand the first time it's loaded
/// (see [`FileSource::resolve`]). Lines with no URL are skipped.
///
/// A `# Platform:<name>` header line applies to every line below it, becoming a
/// `platform` meta on each entry that doesn't name one itself. A header line
/// may carry further `key:value` pairs (`# Platform:Amiga puae_model:A500`), which
/// likewise become meta on every entry below it — see [`parse_db_header`].
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
    collect_db_text(text, filter, out);
    Ok(())
}

/// Turn the raw bytes of a db into its text, unpacking it first when it is a
/// packed file (see [`unpack_if_packed`]). `what` names the source for errors.
///
/// The text is leaked, because the entries [`collect_db_text`] builds from it
/// borrow their fields straight out of it and are kept for the whole run — see
/// [`leak`]. This is the one big leak: a db is read once, and slicing it beats
/// copying every field of every line into its own `String`.
fn db_text(data: Vec<u8>, what: &str) -> Result<&'static str> {
    let data = match unpack_if_packed(data) {
        Ok(data) => data,
        Err(err) => bail!("Failed to unpack {what}: {err}"),
    };
    match String::from_utf8(data) {
        Ok(text) => Ok(leak(text)),
        Err(err) => bail!("Failed to read {what}: {err}"),
    }
}

/// Which db lines to keep, as regexes matched against the raw fields of a line
/// before it is parsed, so a pattern can pick on any field (`category:Demo`,
/// `author:Fairlight`).
///
/// A line has to match *every* `include` pattern and must not match *any*
/// `exclude` pattern, so repeating `-I` narrows the selection down while
/// repeating `-X` widens what is thrown away. Header comments are always read,
/// so the platform and meta they set still apply to the lines that survive.
#[derive(Default)]
pub struct DbFilter<'a> {
    pub include: &'a [Regex],
    pub exclude: &'a [Regex],
}

impl DbFilter<'_> {
    fn keeps(&self, line: &str) -> bool {
        self.include.iter().all(|re| matches_field(re, line))
            && !self.exclude.iter().any(|re| matches_field(re, line))
    }
}

/// Whether `re` matches any one of the line's tab-separated fields.
///
/// Matching field by field rather than the whole line keeps a pattern inside
/// the field it names: `author:.*Firefox` can't run past the end of `author:`
/// and pick up a `Firefox` somewhere later on the line. It also lets `^` and
/// `$` anchor to a field, so `^category:Demo$` picks plain demos while leaving
/// `category:Demoshow` alone. The flip side is that a pattern can no longer
/// span two fields at once.
fn matches_field(re: &Regex, line: &str) -> bool {
    line.split('\t').any(|field| re.is_match(field))
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
    collect_db_text(text, filter, out);
    Ok(())
}

/// Read a header comment such as `# Platform:Amiga puae_model:A500`, which
/// applies to every line below it: `Platform` names the platform the entries
/// are for, and every other pair becomes meta merged into each entry (a
/// db can this way set emulator settings for all its lines at once).
///
/// Only a comment made up entirely of `key:value` pairs is a header, so an
/// ordinary prose comment never turns into meta.
fn parse_db_header(
    comment: &'static str,
    platform: &mut Option<&'static str>,
    meta: &mut HashMap<&'static str, &'static str>,
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
            meta.insert(key, val);
        }
    }
}

/// Parse the contents of a db file — see [`collect_db`] for the format.
pub(crate) fn collect_db_text(text: &'static str, filter: &DbFilter, out: &mut Vec<EmuFile>) {
    let mut file_platform: Option<&'static str> = None;

    // Meta from header comments, applied to every entry below them.
    let mut file_meta = HashMap::<&'static str, &'static str>::new();

    for l in text.lines() {
        let line = l.trim();

        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix('#') {
            parse_db_header(comment, &mut file_platform, &mut file_meta);
            continue;
        }
        if !filter.keeps(line) {
            continue;
        }
        let fields = parse_named_db_line(line);

        let mut meta = file_meta.clone();
        if let Some(platform) = file_platform.filter(|p| !p.is_empty()) {
            // A `platform` field on the line itself is inserted below and wins.
            meta.insert("platform", platform);
        }
        let mut urls = UrlList::default();
        for (key, val) in fields {
            if key == "download" {
                if val.trim().is_empty() {
                    continue;
                }
                urls = UrlList::parse_field(val);
            }
            meta.insert(key, val);
        }
        if urls.is_empty() {
            continue;
        }

        let title = meta.get("title").copied().unwrap_or("");
        let author = meta.get("author").copied().unwrap_or("");
        let category = meta.get("category").copied().unwrap_or("");

        let date = CompactDate::parse(meta.get("date").unwrap_or(&""));

        let year_s = meta
            .get("date")
            .copied()
            .unwrap_or("")
            .split(['-', '/', '.'])
            .next()
            .unwrap_or("");
        meta.insert("year", &year_s);

        let rank = meta
            .get("pouet")
            .copied()
            .and_then(parse_pouet_rank)
            .unwrap_or(0);
        out.push(EmuFile {
            path: FileSource::Url(urls),
            meta,
            game_info: GameInfo {
                title,
                group: author,
                date,
                rank,
                category,
                ..Default::default()
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
        let title = leak(
            in_path
                .file_stem()
                .context("No file stem")?
                .to_string_lossy()
                .into_owned(),
        );
        Ok(EmuFile {
            path: in_path.into(),
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
fn collect_files_(dir: &Path, out: &mut Vec<EmuFile>, many: bool) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            bail!("Failed to read directory {}: {err}", dir.display());
        }
    };
    let mut files = vec![];
    let mut dirs = vec![];
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
            // NOTE: Images on disk are assumed to be screenshots
            if !is_disk_image(&path) {
                disk_images = false;
            }
            files.push(path);
        }
    }

    // Mixed types in directory, add every valid file one by one. A directory
    // holding nothing but disk images is left to the loader, which mounts the
    // whole set rather than each image on its own.
    if many || !disk_images {
        for f in files {
            out.push(collect_file(&f)?);
        }
    }
    for dir in dirs {
        collect_files_(&dir, out, many)?;
    }
    Ok(())
}

pub fn collect_files(dir: &Path, out: &mut Vec<EmuFile>, many: bool) -> Result<()> {
    let len = out.len();
    collect_files_(dir, out, many).and_then(|_| {
        if len == out.len() {
            out.push(collect_file(&dir).unwrap());
        };
        Ok(())
    })
}

#[cfg(test)]
#[path = "tests/files_tests.rs"]
mod tests;
