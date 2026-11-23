//! The latest releases on Demozoo, from its API.
//!
//! The production list carries no download links, and the API allows about two
//! requests a second, so an entry's download is the placeholder
//! `DemozooProduction:<id>`, looked up by [`download_urls`] when it is loaded.

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};
use serde_json::Value;

use crate::emu_file::{EmuFile, FileSource, GameInfo, Meta, UrlList};
use crate::files::leak;

include!(concat!(env!("OUT_DIR"), "/demozoo_key.rs"));

const API: &str = "https://demozoo.org/api/v1/productions/";
const PLACEHOLDER: &str = "DemozooProduction:";
const DAYS: u64 = 14;
const MAX_AGE: Duration = Duration::from_secs(3600);

/// Demozoo's platform names, and the ones demozoo.txt uses for them.
const PLATFORM_NAMES: &[(&str, &str)] = &[
    ("Commodore 64", "C64"),
    ("Commodore 16/Plus 4", "C16"),
    ("Amiga OCS/ECS", "Amiga"),
    ("Amiga AGA", "Amiga AGA"),
    ("Atari ST/E", "Atari ST"),
    ("Atari Falcon", "Atari Falcon"),
    ("Atari 8 bit", "Atari XL"),
    ("Atari 2600 Video Computer System (VCS)", "Atari 2600"),
    ("Atari Lynx", "Atari Lynx"),
    ("Sega Megadrive/Genesis", "Megadrive"),
    ("Nintendo SNES/Super FamiCom", "SNES"),
    ("Nintendo Game Boy (GB)", "Gameboy"),
    ("Nintendo Game Boy Color (GBC)", "Gameboy Color"),
    ("Nintendo Game Boy Advance (GBA)", "GBA"),
    ("Sony Playstation 1 (PSX)", "PlayStation"),
    ("Sony Playstation Portable (PSP)", "PSP"),
    ("TIC-80", "Tic-80"),
    ("PICO-8", "Pico8"),
    ("ZX Spectrum", "ZX Spectrum"),
    ("ZX Spectrum Enhanced", "ZX Next"),
    ("Amstrad CPC", "Amstrad CPC"),
    ("Neo Geo", "Neo Geo"),
    ("MS-Dos", "MS-Dos"),
    ("Windows", "Windows"),
    ("Browser", "Browser"),
];

const CATEGORY_BLACKLIST: &[&str] = &["Tool", "Video"];
const DOWNLOAD_BLACKLIST: &[&str] = &[
    ".php", ".ogg", ".mp4", ".mpg", ".avi", ".wmv", ".mkv", ".pdf", ".wav", ".4q",
];
const SCENE_ORG_VIEW: &str = "https://files.scene.org/view";

static PARSED: Mutex<Option<(Instant, &'static [EmuFile])>> = Mutex::new(None);

/// The Demozoo releases of the last two weeks demarc may be able to play,
/// newest first. Fetched at most once an hour; a failed refresh keeps the last
/// list.
pub fn new_releases() -> Result<&'static [EmuFile]> {
    let mut parsed = PARSED.lock().unwrap();
    if let Some((when, files)) = *parsed
        && when.elapsed() < MAX_AGE
    {
        return Ok(files);
    }
    let mut files = match fetch() {
        Ok(files) => files,
        Err(e) => return parsed.map(|(_, files)| files).ok_or(e),
    };
    files.sort_by_key(|f| std::cmp::Reverse(f.get_meta("date")));
    let files: &'static [EmuFile] = Box::leak(files.into_boxed_slice());
    *parsed = Some((Instant::now(), files));
    Ok(files)
}

fn fetch() -> Result<Vec<EmuFile>> {
    let mut next = Some(format!("{API}?released_since={}", date_days_ago(DAYS)));
    let mut files = vec![];
    while let Some(url) = next {
        let page = get(&url)?;
        files.extend(
            page["results"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(parse_production),
        );
        next = page["next"].as_str().map(String::from);
    }
    Ok(files)
}

/// The production id in a placeholder download.
pub fn production_id(url: &str) -> Option<&str> {
    url.strip_prefix(PLACEHOLDER)
}

/// The downloads of production `id`, best first.
pub fn download_urls(id: &str) -> Result<UrlList> {
    let urls = downloads(&get(&format!("{API}{id}/"))?);
    if urls.as_slice().is_empty() {
        bail!("Demozoo has no download for production {id}");
    }
    Ok(urls)
}

/// The download links of a production's detail json, scene.org's first.
fn downloads(prod: &Value) -> UrlList {
    let mut urls: Vec<String> = prod["download_links"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["url"].as_str())
        .filter(|u| {
            let path = u.split('?').next().unwrap_or(u).to_lowercase();
            !DOWNLOAD_BLACKLIST.iter().any(|ext| path.ends_with(ext))
        })
        .map(|u| match u.strip_prefix(SCENE_ORG_VIEW) {
            Some(path) => format!("SceneOrgFile:{path}"),
            None => u.to_string(),
        })
        .collect();
    urls.sort_by_key(|u| !u.starts_with("SceneOrgFile:"));
    UrlList::parse_field(leak(urls.join(";")))
}

fn api_key() -> String {
    KEY_XOR
        .iter()
        .enumerate()
        .map(|(i, b)| (b ^ 0x5a ^ (i as u8).wrapping_mul(31)) as char)
        .collect()
}

/// `url` as json, waiting out the API's rate limit.
fn get(url: &str) -> Result<Value> {
    let key = api_key();
    if key.is_empty() {
        bail!("built without a Demozoo API key");
    }
    loop {
        let response = ureq::get(url)
            .header("Authorization", &format!("Api-Key {key}"))
            .config()
            .http_status_as_error(false)
            .build()
            .call()?;
        if response.status() == 429 {
            let wait = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok()?.parse().ok())
                .unwrap_or(10);
            std::thread::sleep(Duration::from_secs(wait));
            continue;
        }
        if !response.status().is_success() {
            bail!("{url}: {}", response.status());
        }
        return Ok(serde_json::from_str(
            &response.into_body().read_to_string()?,
        )?);
    }
}

/// The date `days` ago, as `YYYY-MM-DD`.
fn date_days_ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let z = (now / 86400).saturating_sub(days) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn names(list: &Value) -> impl Iterator<Item = &str> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["name"].as_str())
}

fn platforms(prod: &Value) -> Vec<&'static str> {
    names(&prod["platforms"])
        .filter_map(|p| PLATFORM_NAMES.iter().find(|(dz, _)| *dz == p))
        .map(|(_, name)| *name)
        .collect()
}

fn platformless(prod: &Value) -> bool {
    prod["platforms"].as_array().is_none_or(|p| p.is_empty())
        && matches!(prod["supertype"].as_str(), Some("graphics" | "music"))
}

/// Whether a production in the list is one demarc may be able to play.
pub fn is_candidate(prod: &Value) -> bool {
    !names(&prod["types"]).any(|t| CATEGORY_BLACKLIST.contains(&t))
        && (!platforms(prod).is_empty() || platformless(prod))
}

/// A production of the list as an entry, its download a placeholder.
pub fn parse_production(prod: &Value) -> Option<EmuFile> {
    if !is_candidate(prod) {
        return None;
    }
    let id = prod["id"].as_u64()?;
    let urls = UrlList::one(leak(format!("{PLACEHOLDER}{id}")));
    let categories: Vec<&'static str> =
        names(&prod["types"]).map(|t| leak(t.to_string())).collect();
    let meta: Meta = [
        ("id", id.to_string()),
        ("title", prod["title"].as_str()?.to_string()),
        ("author", author(prod)),
        (
            "date",
            prod["release_date"].as_str().unwrap_or("").to_string(),
        ),
        ("tags", names_or_str(&prod["tags"])),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .map(|(k, v)| (k, leak(v)))
    .chain([
        ("db", "Demozoo"),
        ("amiberry_model", "date"),
        ("hatari_machinetype", "date"),
    ])
    .collect();
    Some(EmuFile {
        path: FileSource::Url(urls),
        game_info: GameInfo::new(&meta, &categories),
        meta,
        platforms: platforms(prod).into_boxed_slice(),
        categories: categories.into_boxed_slice(),
    })
}

fn names_or_str(tags: &Value) -> String {
    tags.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(";")
}

/// The byline demozoo.txt gives: the group for a production, the artist for
/// graphics and music, each falling back on the other.
fn author(prod: &Value) -> String {
    let nicks = |list: &Value, group: Option<bool>| -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for nick in list.as_array().into_iter().flatten() {
            let is_group = nick["releaser"]["is_group"].as_bool();
            if let Some(name) = nick["name"].as_str()
                && group.is_none_or(|g| is_group == Some(g))
                && !out.iter().any(|n| n == name)
            {
                out.push(name.to_string());
            }
        }
        out
    };
    let mut groups = nicks(&prod["author_affiliation_nicks"], None);
    if groups.is_empty() {
        groups = nicks(&prod["author_nicks"], Some(true));
    }
    let people = nicks(&prod["author_nicks"], Some(false));
    let (first, second) = match prod["supertype"].as_str() {
        Some("graphics" | "music") => (people, groups),
        _ => (groups, people),
    };
    if first.is_empty() { second } else { first }.join(" & ")
}

#[cfg(test)]
#[path = "tests/demozoo_latest_tests.rs"]
mod tests;
