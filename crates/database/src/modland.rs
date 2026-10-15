//! The modland index (`allmods.txt`): one `<size>\t<path>` line per music file.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use newsys::NewSys;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::{
    emu_file::{EmuFile, FileSource, GameInfo, UrlList},
    files::{DbFilter, leak},
};

pub const MODLAND_BASEURL: &str = "https://modland.com/pub/modules/";

const URL_PATH: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'/')
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

fn index_path(line: &str) -> Option<&str> {
    let (size, path) = line.split_once('\t')?;
    let is_size = !size.is_empty() && size.bytes().all(|b| b.is_ascii_digit());
    (is_size && !path.is_empty() && !path.contains('\t')).then_some(path)
}

pub fn is_index(text: &str) -> bool {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .is_some_and(|l| index_path(l).is_some())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Song<'a> {
    pub platform: &'a str,
    pub author: String,
    pub title: String,
}

/// Parents laid out as `<Author>/<Game>/<file>`; `*` stands for any one
/// directory. The last directory of the parent is the platform.
const GAME_MUSIC: &[&str] = &[
    "Capcom Q-Sound Format",
    "Dreamcast Sound Format",
    "Gameboy Sound Format",
    "Nintendo DS Sound Format",
    "Nintendo SPC",
    "Playstation 2 Sound Format",
    "Playstation Sound Format",
    "Saturn Sound Format",
    "Super Nintendo Sound Format",
    "Ultra64 Sound Format",
    "Video Game Music/*",
];

/// Collections named by their top directory, with underscores for spaces and
/// `<Author>/…/<file>` under the parent, or no author at all.
const COLLECTIONS: &[(&str, bool)] = &[("HVSC/MUSICIANS/*", true), ("HVSC", false), ("SNDH", true)];

fn is_parent(parent: &str, dirs: &[&str]) -> bool {
    dirs.len() >= parent.split('/').count()
        && parent
            .split('/')
            .zip(dirs)
            .all(|(part, dir)| part == "*" || part == *dir)
}

/// How many of `dirs` make up a [`GAME_MUSIC`] parent.
fn game_parent_len(dirs: &[&str]) -> Option<usize> {
    GAME_MUSIC
        .iter()
        .find(|parent| is_parent(parent, dirs))
        .map(|parent| parent.split('/').count())
}

/// The lowercased directory names outside HVSC, which is where authors go by
/// the name [`hvsc_author`] wants to match.
fn other_authors(text: &str) -> HashSet<String> {
    let mut authors = HashSet::new();
    let mut last = "";
    for path in text.lines().filter_map(index_path) {
        let mut dirs = path.split('/');
        if dirs.next() == Some("HVSC") {
            continue;
        }
        dirs.next_back();
        for dir in dirs {
            let dir = dir.strip_prefix("coop-").unwrap_or(dir);
            if dir != last {
                authors.insert(dir.to_lowercase());
                last = dir;
            }
        }
    }
    authors
}

/// HVSC files a person as `Last_First` and a handle as written. Only the
/// reading that is an author elsewhere is trusted to tell them apart.
fn hvsc_author(dir: &str, authors: &HashSet<String>) -> String {
    let plain = dir.replace('_', " ");
    let Some((rest, first)) = plain.rsplit_once(' ') else {
        return plain;
    };
    let swapped = format!("{first} {rest}");
    if !authors.contains(&plain.to_lowercase()) && authors.contains(&swapped.to_lowercase()) {
        swapped
    } else {
        plain
    }
}

/// `.../<Format>/<Author>/<Title>`, matched from the right, or
/// `<parent>/<Author>/<Game>/<file>` under a [`GAME_MUSIC`] parent. A
/// `coop-<Name>` directory adds to the author. [`COLLECTIONS`] go their own way.
pub(crate) fn parse_path<'a>(path: &'a str, authors: &HashSet<String>) -> Song<'a> {
    let mut dirs: Vec<&str> = path.split('/').collect();
    let file = dirs.pop().unwrap_or(path);
    let stem = match file.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => file,
    };
    let mut coop = vec![];
    dirs.retain(|dir| {
        let name = dir.strip_prefix("coop-").or(dir.strip_prefix("coop "));
        coop.extend(name);
        name.is_none()
    });
    let dir = |i: usize| dirs.get(i).copied().unwrap_or("");
    if let Some((parent, has_author)) = COLLECTIONS.iter().find(|(p, _)| is_parent(p, &dirs)) {
        let author = if *has_author {
            dir(parent.split('/').count())
        } else {
            ""
        };
        let author = if dir(0) == "HVSC" {
            hvsc_author(author, authors)
        } else {
            author.replace('_', " ")
        };
        return Song {
            platform: dir(0),
            author: capitalize(&author),
            title: capitalize(&stem.replace('_', " ")),
        };
    }
    let (platform, author, title) = match game_parent_len(&dirs) {
        Some(n) if dirs.len() > n + 1 => {
            let mut sub = dirs[n + 2..].to_vec();
            sub.push(stem);
            (
                dir(n - 1),
                dir(n),
                format!("{} ({})", dir(n + 1), sub.join(" / ")),
            )
        }
        Some(n) => (dir(n - 1), dir(n), capitalize(stem)),
        None => {
            let n = dirs.len();
            (
                dir(n.wrapping_sub(2)),
                dir(n.wrapping_sub(1)),
                capitalize(stem),
            )
        }
    };
    let mut author = capitalize(author.trim_start_matches(['-', ' ']));
    for name in coop {
        author += " & ";
        author += name;
    }
    Song {
        platform,
        author,
        title,
    }
}

pub fn collect(text: &'static str, filter: &DbFilter, out: &mut Vec<EmuFile>) {
    // Every built string goes into one buffer, leaked once and sliced.
    let mut buf = String::new();
    let push = |buf: &mut String, s: &str| -> Range<usize> {
        let start = buf.len();
        buf.push_str(s);
        start..buf.len()
    };
    let extensions = NewSys::supported_extensions();
    let authors = other_authors(text);
    let mut songs = vec![];
    for line in text.lines() {
        let Some(path) = index_path(line) else {
            continue;
        };
        let ext = path.rsplit_once('.').map_or("", |(_, ext)| ext);
        if !extensions.contains(ext.to_ascii_lowercase().as_str()) || !filter.keeps(line) {
            continue;
        }
        let song = parse_path(path, &authors);
        let start = buf.len();
        buf.push_str(MODLAND_BASEURL);
        buf.extend(utf8_percent_encode(path, URL_PATH));
        let url = start..buf.len();
        let title = push(&mut buf, &song.title);
        let author = push(&mut buf, &song.author);
        songs.push((song.platform, url, title, author));
    }

    let buf: &'static str = leak(buf);
    out.reserve(songs.len());
    for (platform, url, title, author) in songs {
        let url = &buf[url];
        let mut meta = HashMap::new();
        meta.insert("download", url);
        meta.insert("title", &buf[title]);
        if !platform.is_empty() {
            meta.insert("platform", platform);
        }
        if !author.is_empty() {
            meta.insert("author", &buf[author]);
        }
        out.push(EmuFile {
            game_info: GameInfo::new(&meta),
            path: FileSource::Url(UrlList::one(url)),
            meta,
        });
    }
}

#[cfg(test)]
#[path = "tests/modland_tests.rs"]
mod tests;
