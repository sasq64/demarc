use std::{
    collections::HashMap,
    fs,
    io::{self, IsTerminal, Read},
    path::Path,
};

use anyhow::{Result, bail};
use regex::Regex;
use tracing::{info, warn};
use url::Url;

use crate::{
    emu_file::{EmuFile, FileSource},
    systems::{GameInfo, SystemType, get_system_type},
    utils::{is_disk_image, parse_m3u, unpack_if_packed},
};

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

/// Parse a `key:value`-per-field line, e.g.
/// `id:1\ttitle:Zentro 4\tauthor:Zenith\t…`. Returns `None` when the line isn't
/// in that format, so the caller can fall back to the positional one.
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

/// Which db lines to keep, as regexes matched against the raw fields of a line
/// before it is parsed, so a pattern can pick on any field (`category:Demo`,
/// `author:Fairlight`).
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
    tags: &mut HashMap<&'a str, &'a str>,
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
            tags.insert(key, val);
        }
    }
}

/// Parse the contents of a db file — see [`collect_db`] for the format.
pub(crate) fn collect_db_text(text: &str, filter: &DbFilter, out: &mut Vec<EmuFile>) {
    let mut file_platform: Option<&str> = None;

    // Tags from header comments, applied to every entry below them.
    let mut file_tags = HashMap::<&str, &str>::new();

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
        let fields = parse_named_db_line(line);

        let mut tags = file_tags.clone();
        let mut urls = vec![];
        for (key, val) in fields {
            if key == "download" {
                if val.trim().is_empty() {
                    continue;
                }
                urls = val
                    .split(';')
                    .filter_map(|p| match Url::parse(p) {
                        Ok(u) => Some(u),
                        Err(err) => {
                            warn!("Skipping unparseable URL {p:?}: {err}");
                            None
                        }
                    })
                    .collect();
            }
            tags.insert(key.into(), val.into());
        }
        if urls.is_empty() {
            continue;
        }

        let title = tags.get("title").copied().unwrap_or("");
        let author = tags.get("author").copied().unwrap_or("");
        let year = tags
            .get("date")
            .copied()
            .unwrap_or("")
            .split(['-', '/', '.'])
            .next()
            .unwrap_or_default()
            .to_string();
        out.push(EmuFile {
            path: FileSource::Url(urls),
            system_type: SystemType::Unknown,
            tags: tags
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
            game_info: GameInfo {
                title: title.into(),
                group: author.into(),
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
        let title = in_path.file_stem().unwrap().to_string_lossy().to_string();
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

#[cfg(test)]
mod tests {
    use crate::emu_file::filter_release_urls;

    use super::*;

    /// The local path behind a source. Everything below collects real files, so
    /// a URL here means the test itself is wrong.
    fn local_path(source: &FileSource) -> &Path {
        match source {
            FileSource::Path(p) => p,
            FileSource::Url(urls) => panic!("expected a local path, got {urls:?}"),
        }
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
                .map(|f| f.game_info.title.clone())
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
}
