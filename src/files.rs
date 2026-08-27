use std::{
    collections::HashMap,
    fs,
    io::{self, IsTerminal, Read},
    path::Path,
};

use anyhow::{Result, bail};
use regex::Regex;
use tracing::info;

use crate::{
    emu_file::{EmuFile, FileSource, GameInfo, UrlList},
    m3u::M3u,
    utils::{is_disk_image, unpack_if_packed},
};

fn handle_m3u(in_path: &Path) -> Result<EmuFile> {
    let mut title: &'static str = "";
    let mut group: &'static str = "";
    let mut year: u32 = 0;
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
            year = val.parse::<u32>().unwrap_or(0);
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
            year,
            category: "",
        },
    })
}

/// Give a runtime-built string the `'static` lifetime an [`EmuFile`] wants.
///
/// The file list is built once and kept for the whole run, so nothing collected
/// into it is ever freed anyway; leaking says so in the type and lets entries
/// hold plain `&'static str` instead of `String`. Only used for the handful of
/// strings that aren't already slices of the leaked db text — m3u tags and file
/// stems — so the leak is bounded by the size of the file list.
fn leak(s: String) -> &'static str {
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
        let year_s = meta
            .get("date")
            .copied()
            .unwrap_or("")
            .split(['-', '/', '.'])
            .next()
            .unwrap_or_default();
        meta.insert("year", year_s);
        let year = year_s.parse::<u32>().unwrap_or(0);
        out.push(EmuFile {
            path: FileSource::Url(urls),
            meta,
            game_info: GameInfo {
                title,
                group: author,
                year,
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
        let title = leak(in_path.file_stem().unwrap().to_string_lossy().into_owned());
        Ok(EmuFile {
            path: in_path.into(),
            //system_type: get_system_type(in_path),
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

    // A Neo Geo CD release ships as the loose files that were meant to be
    // burned onto a disc — the boot list and everything it names. They are one
    // release and mean nothing apart, so the directory becomes a single entry
    // and the loader builds the disc out of it.
    if crate::newsys::holds_boot_list(&files) {
        out.push(collect_file(dir)?);
        return Ok(());
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
        collect_files(&dir, out, many)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::emu_file::filter_release_urls;

    use super::*;

    #[test]
    fn missing_directory_is_an_error() {
        let mut out = vec![];
        assert!(collect_files(Path::new("no/such/dir"), &mut out, false).is_err());
        assert!(out.is_empty());
    }

    /// Every field spells out its name, so order doesn't matter and a value may
    /// hold `:` itself (every URL does). The fields become meta, with the title,
    /// group and year lifted into the entry's `GameInfo`. Blank lines and
    /// URL-less lines are skipped.
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
            urls.as_slice(),
            [
                "http://example.com/zentro4",
                "http://example.com/zentro4.dms"
            ]
        );
        assert_eq!(zentro.game_info.title, "Zentro 4");
        assert_eq!(zentro.game_info.group, "Zenith");
        assert_eq!(zentro.game_info.year, 1992);
        assert_eq!(zentro.get_meta("category"), "Demo");
        assert_eq!(zentro.get_meta("party"), "The Party 1992");
        assert_eq!(zentro.get_meta("tags"), "has effects");

        // Fields in any order, missing ones simply left empty.
        let nexus = &out[1];
        assert_eq!(nexus.game_info.title, "Nexus 7");
        assert_eq!(nexus.game_info.group, "Andromeda");
        assert_eq!(nexus.game_info.year, 1994);
        assert!(!nexus.meta.contains_key("party"));
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
            assert_eq!(eod.game_info.year, 2008, "{packed}");
            // The `# Platform:C64` header applies just as it does unpacked.
            assert_eq!(eod.get_meta("platform"), "C64", "{packed}");
            assert_eq!(eod.get_meta("category"), "demo", "{packed}");
            let FileSource::Url(urls) = &eod.path else {
                panic!("db entries stay URLs until loaded, got {:?}", eod.path)
            };
            assert_eq!(urls.as_slice(), ["https://example.com/eod.d64"], "{packed}");
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
        assert_eq!(out[0].get_meta("category"), "Demo");
        assert!(
            !out[0].meta.contains_key("platform"),
            "no header, no platform"
        );
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
                    .map(|f| f.game_info.title)
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
        assert_eq!(
            out[0].get_meta("platform"),
            "Amiga",
            "header still applies"
        );

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

    /// Each pattern is matched against one field at a time, so it can't run
    /// past the end of the field it names into the next one, and `^`/`$`
    /// anchor to a field rather than the whole line.
    #[test]
    fn collect_db_filter_matches_per_field() {
        const DB: &str = "id:1\ttitle:Firefox Intro\tauthor:Zenith\tcategory:Demo\tdownload:http://example.com/a\n\
             id:2\ttitle:Hoax\tauthor:Firefox\tcategory:Demoshow\tdownload:http://example.com/b\n";

        let titles = |filter: &DbFilter| {
            let mut out = vec![];
            collect_db_text(DB, filter, &mut out);
            out.iter()
                .map(|f| f.game_info.title)
                .collect::<Vec<_>>()
        };

        // `.*` stops at the field end, so the `Firefox` in the title of the
        // first line doesn't count as an author.
        let include = [Regex::new("author:.*Firefox").unwrap()];
        assert_eq!(
            titles(&DbFilter {
                include: &include,
                ..Default::default()
            }),
            ["Hoax"]
        );

        // `$` is the end of a field, so `Demoshow` isn't a `Demo`.
        let include = [Regex::new("^category:Demo$").unwrap()];
        assert_eq!(
            titles(&DbFilter {
                include: &include,
                ..Default::default()
            }),
            ["Firefox Intro"]
        );

        // A pattern spanning the tab between two fields matches nothing.
        let include = [Regex::new("title:Hoax\tauthor:Firefox").unwrap()];
        assert!(
            titles(&DbFilter {
                include: &include,
                ..Default::default()
            })
            .is_empty()
        );
    }

    /// A `# Platform:` header covers the lines below it, giving each one a
    /// `platform` tag, and a line naming a platform of its own overrides it —
    /// that way entries from different scenes stay apart.
    #[test]
    fn collect_db_platform_tags_lines() {
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
        assert_eq!(
            out[0].get_meta("platform"),
            "Amiga",
            "header applies"
        );
        assert_eq!(out[0].get_meta("category"), "Demo");
        assert_eq!(
            out[1].get_meta("platform"),
            "C64",
            "line overrides header"
        );
    }

    /// The other pairs of a header line become meta on every entry below it,
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

        assert_eq!(out[0].get_meta("platform"), "Amiga");
        assert_eq!(out[0].get_meta("puae_model"), "A500");
        assert!(!out[0].meta.contains_key("Just"));
        assert!(!out[0].meta.contains_key("comment"));

        // A later header overrides, and the platform from the first one still
        // applies.
        assert_eq!(out[1].get_meta("puae_model"), "A1200");
        assert_eq!(out[1].get_meta("tags"), "aga");
        assert_eq!(out[1].get_meta("platform"), "Amiga");
    }

    /// A disk image among the URLs makes the release disk based: everything
    /// that isn't a disk image goes away and the whole set stays, whatever
    /// format its disks are in.
    #[test]
    fn disk_images_win() {
        assert_eq!(
            filter_release_urls(&[
                "https://x.com/a.pdf",
                "https://x.com/a1.d64",
                "https://x.com/a2.D64",
                "https://x.com/a.adf",
                "https://x.com/readme.txt",
            ]),
            vec![
                "https://x.com/a1.d64",
                "https://x.com/a2.D64",
                "https://x.com/a.adf"
            ]
        );
    }

    /// Without a disk image only the known extras are dropped; everything else,
    /// including extension-less URLs, is left for the caller to sort out.
    #[test]
    fn extras_dropped() {
        assert_eq!(
            filter_release_urls(&[
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
            filter_release_urls(&["https://x.com/a.sid", "https://x.com/b.pdf"]),
            vec!["https://x.com/a.sid", "https://x.com/b.pdf"]
        );
    }
}
