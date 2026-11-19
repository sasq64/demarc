//! The latest releases on CSDb, scraped from its `latestreleases.php` page.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::emu_file::{EmuFile, FileSource, GameInfo, Meta, UrlList, split_values};
use crate::files::leak;

const URL: &str = "https://csdb.dk/latestreleases.php?count=500";
const BASE: &str = "https://csdb.dk";
/// The `release_url` and `release_css` csdb.txt's header gives its entries.
pub const RELEASE_URL: &str = "https://csdb.dk/release/?id={id}";
pub const RELEASE_CSS: &str = "csdb.css";

const MAX_AGE: Duration = Duration::from_secs(3600);

/// The csdb categories that are something to watch.
pub const CATEGORIES: &[&str] = &[
    "Demo",
    "One-File Demo",
    "Intro",
    "Crack Intro",
    "4K Intro",
    "1K Intro",
    "256b Intro",
    "Basic Demo",
    "Fake Demo",
    "Invitation",
    "Diskmag",
    "Intro Collection",
    "REU Release",
    "EasyFlash Release",
    "Graphics",
    "Graphics Collection",
    "Music",
    "Music Collection",
];

static PARSED: Mutex<Option<(Instant, &'static [EmuFile])>> = Mutex::new(None);

/// The latest watchable CSDb releases that have a download link, newest first.
/// Fetched at most once an hour; a failed refresh keeps the last list.
pub fn new_releases() -> Result<&'static [EmuFile]> {
    let mut parsed = PARSED.lock().unwrap();
    if let Some((when, files)) = *parsed
        && when.elapsed() < MAX_AGE
    {
        return Ok(files);
    }
    let html = match ureq::get(URL)
        .call()
        .and_then(|r| r.into_body().read_to_string())
    {
        Ok(html) => html,
        Err(e) => return parsed.map(|(_, files)| files).ok_or(e.into()),
    };
    let files: &'static [EmuFile] = Box::leak(parse(&html).into_boxed_slice());
    *parsed = Some((Instant::now(), files));
    Ok(files)
}

/// The rows of the release table in a `latestreleases.php` page.
pub fn parse(html: &str) -> Vec<EmuFile> {
    let Some(start) = html.find("<b>Release Date</b>") else {
        return vec![];
    };
    let table = &html[start..];
    let table = &table[..table.find("</table>").unwrap_or(table.len())];
    table.split("<tr>").skip(1).filter_map(parse_row).collect()
}

fn parse_row(row: &str) -> Option<EmuFile> {
    let cells: Vec<&str> = row.split("<td").skip(1).collect();
    let [dl, name, kind, by, date, ..] = cells[..] else {
        return None;
    };
    let download = format!("{BASE}{}", href(dl)?);
    let id = href(name)?.rsplit("id=").next()?;
    let category = text(kind);
    let category = category.strip_prefix("C64 ").unwrap_or(&category);
    if !CATEGORIES.contains(&category) {
        return None;
    }
    let authors: Vec<String> = by
        .split("</a>")
        .map(text)
        .filter(|a| !a.is_empty())
        .collect();
    let meta: Meta = [
        ("id", leak(id.to_string())),
        ("title", leak(text(name))),
        ("author", leak(authors.join(" & "))),
        ("date", leak(text(date))),
        ("db", "CSDb"),
        ("release_url", RELEASE_URL),
        ("release_css", RELEASE_CSS),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    let categories = split_values(leak(category.to_string()));
    Some(EmuFile {
        path: FileSource::Url(UrlList::from(vec![leak(download)])),
        game_info: GameInfo::new(&meta, &categories),
        meta,
        platforms: Box::new(["C64"]),
        categories,
    })
}

/// The first link target in `cell`.
fn href(cell: &str) -> Option<&str> {
    let rest = &cell[cell.find("href=\"")? + 6..];
    Some(&rest[..rest.find('"')?])
}

/// `cell` with its tags dropped and entities decoded.
fn text(cell: &str) -> String {
    let cell = cell.find('>').map_or(cell, |i| &cell[i + 1..]);
    let mut out = String::new();
    let mut in_tag = false;
    for c in cell.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(out.trim())
}

/// The named entities for U+00A0 to U+00FF, in order.
const LATIN1: [&str; 96] = [
    "nbsp", "iexcl", "cent", "pound", "curren", "yen", "brvbar", "sect", "uml", "copy", "ordf",
    "laquo", "not", "shy", "reg", "macr", "deg", "plusmn", "sup2", "sup3", "acute", "micro",
    "para", "middot", "cedil", "sup1", "ordm", "raquo", "frac14", "frac12", "frac34", "iquest",
    "Agrave", "Aacute", "Acirc", "Atilde", "Auml", "Aring", "AElig", "Ccedil", "Egrave", "Eacute",
    "Ecirc", "Euml", "Igrave", "Iacute", "Icirc", "Iuml", "ETH", "Ntilde", "Ograve", "Oacute",
    "Ocirc", "Otilde", "Ouml", "times", "Oslash", "Ugrave", "Uacute", "Ucirc", "Uuml", "Yacute",
    "THORN", "szlig", "agrave", "aacute", "acirc", "atilde", "auml", "aring", "aelig", "ccedil",
    "egrave", "eacute", "ecirc", "euml", "igrave", "iacute", "icirc", "iuml", "eth", "ntilde",
    "ograve", "oacute", "ocirc", "otilde", "ouml", "divide", "oslash", "ugrave", "uacute", "ucirc",
    "uuml", "yacute", "thorn", "yuml",
];

fn decode_entities(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e < 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let decoded = match &rest[1..end] {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if let Some(i) = LATIN1.iter().position(|&n| n == e) => {
                char::from_u32(160 + i as u32)
            }
            e => e
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| e.strip_prefix('#')?.parse().ok())
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[path = "tests/csdb_latest_tests.rs"]
mod tests;
