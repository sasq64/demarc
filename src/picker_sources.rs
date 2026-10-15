//! The [`FuzzySource`]s behind the file and download pickers.

use crate::emu_file::{EmuFile, FileSource, UrlList};
use crate::fuzzy_list::{FuzzySource, IndexedSource, ListIcon};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconMode {
    Platforms,
    Categories,
    PlatformOnly,
    CategoryOnly,
    All,
}

/// Backs the file picker: an [`IndexedSource`] over the one-line names shown in
/// the list, paired with the entries themselves — the fuller detail (year,
/// type, party, …) shown in the info field below the list, and the entry a
/// selection is *of*, handed back by [`FuzzySource::get_data`].
#[derive(Clone)]
pub struct PickerSource {
    mode: IconMode,
    names: IndexedSource,
    /// Index into `emu_files` per row, in the order `names` holds them: the ids
    /// a search reports are rows of this subset, not of the whole list. `None`
    /// means every entry, in order, so a row id *is* its index.
    subset: Option<Vec<u32>>,
    emu_files: &'static [EmuFile],
    width: u32,
}

impl PickerSource {
    pub(crate) fn new(
        files: &'static [EmuFile],
        subset: Option<Vec<u32>>,
        icon_mode: IconMode,
    ) -> Self {
        let names: Vec<String> = match &subset {
            Some(subset) => subset
                .iter()
                .map(|&index| entry_name(&files[index as usize]))
                .collect(),
            None => files.iter().map(entry_name).collect(),
        };
        Self {
            names: IndexedSource::new(names),
            subset,
            emu_files: files,
            width: 70,
            mode: icon_mode,
        }
    }

    fn index(&self, id: usize) -> Option<usize> {
        match &self.subset {
            Some(subset) => subset.get(id).map(|&i| i as usize),
            None => Some(id),
        }
    }

    fn file(&self, id: usize) -> Option<&EmuFile> {
        self.emu_files.get(self.index(id)?)
    }
}

impl FuzzySource<EmuFile> for PickerSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        self.file(id)
            .map(|file| entry_info(file, self.width as usize))
            .unwrap_or_default()
    }

    fn get_data(&self, id: usize) -> Option<EmuFile> {
        self.file(id).cloned()
    }

    fn get_item(&self, id: usize) -> usize {
        self.index(id).unwrap_or(id)
    }

    fn file_index(&self, id: usize) -> Option<usize> {
        self.index(id)
    }
    fn get_icon(&self, id: usize) -> (Option<ListIcon>, Option<ListIcon>) {
        let Some(file) = self.file(id) else {
            return (None, None);
        };
        let p = file.get_meta("platform").split(';').next().unwrap_or("");
        let c = file.get_meta("category").split(';').next().unwrap_or("");
        let category = ListIcon::Glyph(category_icon(c), category_color(c));
        let platform = ListIcon::Glyph(platform_icon(p), platform_color(p));
        match self.mode {
            IconMode::Categories | IconMode::Platforms => (Some(category), Some(platform)),
            IconMode::CategoryOnly => (Some(category), None),
            IconMode::PlatformOnly => (None, Some(platform)),
            IconMode::All => (None, None),
        }
    }
}

fn category_icon(category: &str) -> char {
    // match category {
    //     "Music" => '',
    //     "Graphics" => '',
    //     "Tool" | "Other Platform C64 Tool" => '󱁤',
    //     "Game" | "Game Preview" => '󰊖',
    //     "Crack" => '󰋮',
    //     "Demo" | "Intro" | "4K Intro" | "8K Intro" | "64K Intro" | "40k Intro" | "256b Intro"
    //     | "128b Intro" | "64b Intro" => '󱄄',
    //     "Invitation" => '󰺻',
    //     _ => '󰧯',
    // }
    match category {
        "Music" => '󰺢',
        "Streaming Music" => '',
        "Graphics" => '',
        "Tool" | "Other Platform C64 Tool" => '󱁤',
        "Game" | "Game Preview" => '󰯽',
        "Crack" => '󰯱',
        "Intro" => '󰰃',
        "Demo" => '󰯴',
        "4K Intro" => '󰎮',
        "1K Intro" => '󰎦',
        "8K Intro" => '󰎻',
        "32K Intro" => '󰰃',
        "64K Intro" => '󰰃',
        "40k Intro" => '󰰃',
        "256b Intro" => '󰎩',
        "128b Intro" | "64b Intro" => '󰰃',
        "Invitation" => '󰰪',
        _ => '󰗮',
    }
}

fn category_color(category: &str) -> u32 {
    match category {
        "Music" => 0x40c0f0,
        "Graphics" => 0xf0c0f0,
        "Demo" => 0x80fffe,
        "Intro" => 0xa06060,
        "4K Intro" => 0x60e060,
        "8K Intro" => 0x60e0e0,
        "64K Intro" => 0xb07070,
        "40K Intro" => 0xb08050,
        "Crack" => 0x808080,
        "256b Intro" | "128b Intro" | "64b Intro" => 0x905090,
        "Invitation" => 0xe0e050,
        _ => 0xff00ff00,
    }
}

fn platform_icon(platform: &str) -> char {
    match platform {
        "Windows" => '',
        "MS-Dos" => '',
        "Atari 2600" | "SNES" | "Neo Geo" => '',
        "ZX Spectrum" => '󰨛',
        "Gameboy" | "GBA" | "Lynx" => '󱎓',
        "Amiga" | "Amiga AGA" | "Atari ST" => '󰉉',
        "C16" | "Amstrad CPC" | "C64" | "Atari XL" => '󰧯',
        "PlayStation" | "PSP" | "Megadrive" => '󰊖',
        _ => ' ',
    }
}

fn platform_color(platform: &str) -> u32 {
    match platform {
        "Windows" => 0x40c0f0,
        "MS-Dos" => 0xe09090,
        "Atari 2600" | "SNES" | "Gameboy" | "GBA" => 0xa0a0a0,
        "ZX Spectrum" => 0xe0e030,
        "Amiga" | "Amiga AGA" | "C64" => 0xe0e070,
        "Atari ST" | "Atari XL" => 0xe08080,
        "C16" => 0x906060,
        "Amstrad CPC" => 0x40a070,
        "PlayStation" | "PSP" | "Megadrive" => 0x20df30,
        _ => 0xff00ff00,
    }
}

#[derive(Clone)]
pub struct DownloadSource {
    names: IndexedSource,
    downloads: Vec<&'static str>,
    emu_file: EmuFile,
    width: u32,
}

impl DownloadSource {
    pub fn new(emu_file: &EmuFile) -> Self {
        let downloads: Vec<&'static str> = emu_file.get_meta("download").split(";").collect();
        let names: Vec<String> = emu_file
            .get_meta("download")
            .split(";")
            .map(|s| s.rsplit('/').next().unwrap_or(s))
            .map(|s| s.to_string())
            .collect();
        Self {
            names: IndexedSource::new(names),
            downloads,
            emu_file: emu_file.clone(),
            width: 72,
        }
    }
}

impl FuzzySource<EmuFile> for DownloadSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        let mut emu_file = self.emu_file.clone();
        emu_file.meta.insert("download", self.downloads[id]);
        emu_file.path = FileSource::Url(UrlList::one(self.downloads[id]));
        entry_info(&emu_file, self.width as usize)
    }

    fn get_data(&self, id: usize) -> Option<EmuFile> {
        let mut emu_file = self.emu_file.clone();
        emu_file.meta.insert("download", self.downloads[id]);
        emu_file.path = FileSource::Url(UrlList::one(self.downloads[id]));
        Some(emu_file)
    }

    fn get_item(&self, id: usize) -> usize {
        id
    }
}

/// Shorten `url` to at most `max` characters by dropping path components from
/// the left, keeping the two parts that identify it — the host it came from and
/// the file name at the end. Everything dropped is replaced by a single `...`:
///
/// `https://ftp.example.org/pub/demos/c64/1992/zentro4.zip`
/// → `https://ftp.example.org/.../1992/zentro4.zip`
/// → `https://ftp.example.org/.../zentro4.zip`
///
/// A URL still too long once every component is gone has nothing left to drop,
/// so it is cut out of the middle instead, keeping its head and the end of the
/// file name (extension included).
fn trunc_url(url: &str, max: usize) -> String {
    if url.chars().count() <= max {
        return url.to_string();
    }

    // Split into `scheme://host` and the path below it. The path search starts
    // after `://` so the scheme's own slashes don't count as the first one.
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    let (host, path) = match url[after_scheme..].find('/') {
        Some(i) => url.split_at(after_scheme + i),
        // No path at all: there is nothing to drop, only the middle cut below.
        None => (url, ""),
    };
    let (dirs, file) = match path.rsplit_once('/') {
        Some((dirs, file)) => (dirs.trim_start_matches('/'), file),
        None => ("", ""),
    };
    let dirs: Vec<&str> = if dirs.is_empty() {
        Vec::new()
    } else {
        dirs.split('/').collect()
    };

    // Drop one more leading component per round until what's left fits.
    for skip in 1..=dirs.len() {
        let kept = dirs[skip..].join("/");
        let candidate = if kept.is_empty() {
            format!("{host}/.../{file}")
        } else {
            format!("{host}/.../{kept}/{file}")
        };
        if candidate.chars().count() <= max {
            return candidate;
        }
    }

    middle_cut(&format!("{host}/.../{file}"), max)
}

/// Cut `s` down to `max` characters by removing from the middle, so both ends
/// stay readable. Used as [`trunc_url`]'s last resort.
fn middle_cut(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return chars.iter().take(max).collect();
    }
    let keep = max - 3;
    let front = keep / 2;
    let back = keep - front;
    let head: String = chars[..front].iter().collect();
    let tail: String = chars[chars.len() - back..].iter().collect();
    format!("{head}...{tail}")
}

/// The single line an entry gets in the picker list: `title / group`.
fn entry_name(file: &EmuFile) -> String {
    let info = &file.game_info;
    if info.title.is_empty() {
        "???".into()
    } else if info.group.is_empty() {
        info.title.to_string()
    } else {
        format!("{} / {}", info.title, info.group)
    }
}

/// Everything we know about an entry, for the picker's info field: title,
/// group, what it is and when, the party it was released at, its tags, and
/// where it comes from. Empty fields are left out rather than shown blank.
fn entry_info(file: &EmuFile, width: usize) -> String {
    let mut lines = Vec::new();
    let platform = file.get_meta("platform");
    let category = file.get_meta("category");
    let id = file.get_meta("id");
    let year = file.game_info.year();
    let year = if year == 0 {
        "".to_string()
    } else {
        format!(" ({year})")
    };
    if platform.is_empty() {
        lines.push(format!("#{id} {category}{year}"));
    } else {
        lines.push(format!("#{id} {platform} {category}{year}"));
    }
    if let Some(party) = file.meta.get("party").filter(|p| !p.is_empty()) {
        lines.push(format!("Party: {party}"));
    }
    if let Some(tags) = file.meta.get("tags").filter(|t| !t.is_empty()) {
        lines.push(format!("Tags: {tags}"));
    }

    let source = match &file.path {
        FileSource::Path(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
        FileSource::Url(urls) => urls
            .first()
            .map(|u| trunc_url(u, width))
            .unwrap_or_default(),
    };
    if !source.is_empty() {
        lines.push(source);
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "tests/picker_sources_tests.rs"]
mod tests;
