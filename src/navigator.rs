use std::collections::{BTreeSet, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use bevy::prelude::*;
use indexmap::{IndexMap, IndexSet};
use regex::Regex;

use crate::commands::{DownloadSource, IconMode, PickerSource};
use crate::config::AppSettings;
use crate::egui_ui::HudState;
use crate::emu_file::EmuFile;
use crate::fuzzy_list::{AllWordsSource, FuzzySource, ListIcon, WordsIconSource};
use crate::loading::LoadFile;
use crate::playlists::{FAVORITES, Playlists};
use crate::ui::{FuzzyListSelect, ListSource, ShowFuzzyList};

/// Icons for the root list, as nerd font glyphs: the favorites playlist, the
/// other playlists, and the databases.
const FAVORITES_ICON: ListIcon = ListIcon::Glyph('\u{f004}', 0xff4040);
const PLAYLIST_ICON: ListIcon = ListIcon::Glyph('\u{f0cb9}', 0x4080ff);
const DATABASE_ICON: ListIcon = ListIcon::Glyph('\u{f01bc}', 0xa0d8ff);

const PARTY_ICON: char = '\u{f1056}';
/// Saturation and brightness of the per-party colours; tweak to taste.
const PARTY_SATURATION: f32 = 0.65;
const PARTY_VALUE: f32 = 0.75;

/// Colour a party row by hashing its name without the trailing year, so all
/// editions of the same party share a colour.
fn party_icon(name: &str) -> ListIcon {
    let base = name
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .trim_end();
    let mut hasher = DefaultHasher::new();
    base.hash(&mut hasher);
    let hue = (hasher.finish() % 360) as f32;
    ListIcon::Glyph(PARTY_ICON, hsv_to_rgb(hue, PARTY_SATURATION, PARTY_VALUE))
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let chan = |n: f32| {
        let k = (n + h / 60.0) % 6.0;
        let c = v - v * s * k.min(4.0 - k).clamp(0.0, 1.0);
        (c * 255.0).round() as u32
    };
    chan(5.0) << 16 | chan(3.0) << 8 | chan(1.0)
}

pub(crate) struct NavList {
    id: usize,
    source: ListSource,
    path: String,
    /// Search text this list was left with, restored when it comes back up.
    prompt: String,
    /// Source id of the row this list was left on.
    selected: Option<usize>,
}

impl NavList {
    pub(crate) fn show(&self, show_list: &mut MessageWriter<ShowFuzzyList>) {
        show_list.write(ShowFuzzyList {
            id: self.id,
            source: self.source.clone(),
            prompt: Some(self.prompt.clone()),
            selected: self.selected,
            title: self.path.clone(),
        });
    }
}

/// The list an entry was launched from, in the order it was shown, so
/// NextFile/PrevFile walk it instead of the whole db.
pub(crate) struct Launch {
    source: ListSource,
    ids: Vec<usize>,
    pub(crate) index: isize,
}

impl Launch {
    /// Move `d` steps and return the entry there, with its index into the db.
    pub(crate) fn step(&mut self, d: isize, wrap: bool) -> Option<(EmuFile, Option<usize>)> {
        let len = self.ids.len() as isize;
        let mut index = self.index + d;
        if wrap && len > 0 {
            index = index.rem_euclid(len);
        }
        let &id = self.ids.get(usize::try_from(index).ok()?)?;
        let file = self.source.get_data(id)?;
        self.index = index;
        Some((file, self.source.file_index(id)))
    }
}

const PLAYLIST_MENU: usize = 1;

type DbCallback = Box<dyn Fn(&[&str], &'static [EmuFile]) -> ListSource + Send + Sync>;

#[derive(Resource)]
pub(crate) struct Navigator {
    pub(crate) pos: isize,
    showing: isize,
    pub(crate) stack: Vec<NavList>,
    files: IndexMap<String, &'static [EmuFile]>,
    mapping: Vec<(Regex, DbCallback)>,
    playlists: HashSet<String>,
    pub(crate) current_launch: Option<Launch>,
}

impl Navigator {
    pub(crate) fn new() -> Self {
        Self {
            pos: -1,
            showing: -1,
            files: IndexMap::new(),
            stack: vec![],
            mapping: Vec::new(),
            playlists: HashSet::new(),
            current_launch: None,
        }
    }
    fn push(&mut self, nav_list: NavList) -> &mut Self {
        self.pos += 1;
        self.stack.truncate(self.pos as usize);
        self.stack.push(nav_list);
        self
    }
    fn back(&mut self) -> &mut Self {
        if self.pos > 0 {
            self.pos -= 1;
        }
        self
    }
    fn forward(&mut self) -> &mut Self {
        if self.pos < self.stack.len() as isize - 1 {
            self.pos += 1;
        }
        self
    }

    /// Take the search text and selection out of the open list, so going back
    /// to this level later restores them.
    fn remember_state(&mut self, hud: &HudState) {
        if self.pos < 0 {
            return;
        }
        let list = &mut self.stack[self.pos as usize];
        if let Some(query) = hud.list_query(list.id) {
            if list.prompt != query {
                list.prompt = query.into();
            }
            list.selected = hud.list_selected_item(list.id);
        }
    }

    /// Index into the `[EmuFile]` array of the selected entry in the current list.
    pub(crate) fn selected_file_index(&self) -> Option<usize> {
        let list = self.stack.get(usize::try_from(self.pos).ok()?)?;
        debug!(
            "Selected {} {:?}",
            list.selected?,
            list.source.file_index(list.selected?)
        );
        list.source.file_index(list.selected?)
    }
    pub(crate) fn next_index(&self) -> Option<usize> {
        let list = self.stack.get(usize::try_from(self.pos + 1).ok()?)?;
        list.source.file_index(list.selected?)
    }

    pub(crate) fn show(&mut self, lw: &mut MessageWriter<ShowFuzzyList>) {
        if self.pos != self.showing {
            self.stack[self.pos as usize].show(lw);
            self.showing = self.pos;
        }
    }

    fn root_source(&self) -> ListSource {
        Arc::new(WordsIconSource::new(
            self.files
                .keys()
                .map(|name| {
                    let icon = if name == FAVORITES {
                        FAVORITES_ICON
                    } else if self.playlists.contains(name) {
                        PLAYLIST_ICON
                    } else {
                        DATABASE_ICON
                    };
                    (name.clone(), icon)
                })
                .collect(),
        ))
    }

    fn go_root(&mut self) -> &mut Self {
        self.stack.clear();
        self.pos = 0;
        self.stack.push(NavList {
            id: 0,
            source: self.root_source(),
            path: "".into(),
            prompt: String::new(),
            selected: None,
        });
        self
    }

    fn goto(&mut self, path: &str) -> &mut Self {
        if path.is_empty() {
            return self.go_root();
        }
        debug!("Goto: '{}'", path);
        let (db, rest) = path.split_once('/').unwrap_or((path, ""));
        let Some(files) = self.files.get(db).copied() else {
            warn!("No such database: {db}");
            return self;
        };
        if rest.is_empty() && self.playlists.contains(db) {
            self.push(NavList {
                id: 0,
                source: Arc::new(PickerSource::new(files, None, IconMode::All)),
                path: path.into(),
                prompt: String::new(),
                selected: None,
            });
            return self;
        }
        for (key, val) in &self.mapping {
            if let Some(m) = key.captures(rest) {
                let groups: Vec<&str> = m.iter().map(|m| m.map_or("", |m| m.as_str())).collect();
                let source = val(&groups, files);
                self.push(NavList {
                    id: 0,
                    source,
                    path: path.into(),
                    prompt: String::new(),
                    selected: None,
                });
                return self;
            }
        }
        self
    }

    fn current_path(&self) -> &str {
        if self.pos < 0 {
            return "";
        }
        &self.stack[self.pos as usize].path
    }

    fn enter(&mut self, path: &str) -> &mut Self {
        let current = self.current_path().to_string();
        if current.is_empty() {
            return self.goto(path);
        }
        self.goto(&(current + "/" + path))
    }

    #[cfg(test)]
    fn get_showing(&self) -> Vec<String> {
        if self.pos < 0 {
            return vec![];
        }
        self.stack[self.pos as usize].source.get_all_strings()
    }

    /// Add a top level entry to the Navigator. It must be backed by a static
    /// list of EmuFiles that other Navigator parts filter from
    pub fn add_db(&mut self, name: &str, files: &'static [EmuFile]) {
        self.files.insert(name.to_string(), files);
    }

    /// Add a db whose top level is the list of its entries, or update one.
    pub fn add_playlist(&mut self, name: &str, files: &'static [EmuFile]) {
        let new = self.playlists.insert(name.to_string());
        self.add_db(name, files);
        let root_source = self.root_source();
        if new && let Some(root) = self.stack.first_mut() {
            root.source = root_source;
        }
        for list in &mut self.stack {
            if list.id == 0 && list.path == name {
                list.source = Arc::new(PickerSource::new(files, None, IconMode::All));
            }
        }
    }

    /// Shift+Enter on entry `id` of a playlist: remove it, or go on to its
    /// downloads.
    fn open_playlist_menu(&mut self, id: usize) -> &mut Self {
        let list = self.current_path().to_string();
        self.push(NavList {
            id: PLAYLIST_MENU,
            source: Arc::new(AllWordsSource::new(vec![
                format!("\u{f0156} Remove from {list}"),
                "Run...".into(),
            ])),
            path: format!("{list}/{id}"),
            prompt: String::new(),
            selected: None,
        })
    }

    // Add a new path pattern.
    // 'callback' will be called to populate navitator if 'pattern' matches current path.
    // ie: "DemoZoo/Parties/Revision 2022" will match "Parties\/([^\/]*)\" and the callback
    // will be called with ["DemoZoo/Parties/Revision 2022", "Revision 2022"] and the EmuFiles
    // added for database "Demozoo"
    pub fn register_regex<S: FuzzySource<EmuFile>>(
        &mut self,
        pattern: Regex,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> S + Send + Sync + 'static,
    ) -> Result<()> {
        debug!("Regex: {pattern:?}");
        self.mapping.push((
            pattern,
            Box::new(move |groups, files| Arc::new(callback(groups, files))),
        ));
        Ok(())
    }

    // Register with simpler pattern; "Parties/{party}/{combo}" becomes "^Parties/([^/]*)/([^/]*)$".
    // A "*" part matches any number of leading path components, so "*/{id}/dls" also
    // matches "Platforms/C64/12345/dls".
    pub fn register<S: FuzzySource<EmuFile>>(
        &mut self,
        pattern: &str,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> S + Send + Sync + 'static,
    ) -> Result<()> {
        let mut rx = String::from("^");
        let mut sep = false;
        for part in pattern.split('/') {
            if part == "*" {
                rx.push_str("(?:[^/]+/)*");
                sep = false;
                continue;
            }
            if sep {
                rx.push('/');
            }
            if part.starts_with('{') && part.ends_with('}') {
                rx.push_str("([^/]*)");
            } else {
                rx.push_str(&regex::escape(part));
            }
            sep = true;
        }
        rx.push('$');
        self.register_regex(Regex::new(&rx)?, callback)
    }
}

static CATS: [&str; 10] = [
    "Demo",
    "One-File Demo",
    "Intro",
    "64K Intro",
    "4K Intro",
    "256b Intro",
    "1K Intro",
    "Graphics",
    "Music",
    "Tracked Music",
];

pub fn setup_navigator(
    dbs: &IndexMap<String, &'static [EmuFile]>,
    navigator: &mut Navigator,
) -> Result<()> {
    for (name, files) in dbs {
        navigator.add_db(name, files);
    }

    navigator.register("All", |_path: &[&str], files: &'static [EmuFile]| {
        PickerSource::new(files, None, IconMode::All)
    })?;
    navigator.register("Parties", |_path: &[&str], files: &'static [EmuFile]| {
        WordsIconSource::new(
            files
                .iter()
                .map(|f| f.get_party())
                .filter(|p| !p.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|p| (p.to_string(), party_icon(p)))
                .collect(),
        )
    })?;
    navigator.register("Platforms", |_path: &[&str], files: &'static [EmuFile]| {
        AllWordsSource::new(
            files
                .iter()
                .map(|f| f.get_meta("platform"))
                .filter(|p| !p.is_empty() && !p.contains(";"))
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        )
    })?;
    navigator.register("Categories", |_path: &[&str], files: &'static [EmuFile]| {
        let mut cats: Vec<String> = files
            .iter()
            .map(|f| f.get_meta("category"))
            .filter(|p| !p.is_empty() && !p.contains(";"))
            .collect::<IndexSet<_>>()
            .into_iter()
            .map(String::from)
            .collect();
        // Known categories in CATS order, the rest after in the order they appeared.
        cats.sort_by_key(|c| CATS.iter().position(|n| n == c).unwrap_or(CATS.len()));
        AllWordsSource::new(cats)
    })?;
    navigator.register(
        "Platforms/{platform}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.get_meta("platform") == path[1])
                .map(|(i, _)| i as u32)
                .collect();
            PickerSource::new(files, Some(subset), IconMode::Categories)
        },
    )?;

    navigator.register(
        "Categories/{category}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.get_meta("category") == path[1])
                .map(|(i, _)| i as u32)
                .collect();
            PickerSource::new(files, Some(subset), IconMode::Platforms)
        },
    )?;
    navigator.register(
        "Parties/{name}",
        |path: &[&str], files: &'static [EmuFile]| {
            println!("PATH: {}", path[1]);
            AllWordsSource::new(
                files
                    .iter()
                    .filter(|f| f.get_party() == path[1])
                    .map(|f| f.get_compo())
                    .filter(|p| !p.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(String::from)
                    .collect(),
            )
        },
    )?;

    navigator.register(
        "Parties/{name}/{compo}",
        |path: &[&str], files: &'static [EmuFile]| {
            let mut subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.get_party_and_compo() == (path[1], path[2]))
                .map(|(i, _)| i as u32)
                .collect();
            subset.sort_by_key(|i| files[*i as usize].get_numeric_place());
            PickerSource::new(files, Some(subset), IconMode::Platforms)
        },
    )?;

    navigator.register("", |_path: &[&str], _files: &'static [EmuFile]| {
        return WordsIconSource::new(
            [
                ("All".into(), ListIcon::Glyph('\u{f069}', 0xffff00)),
                ("Parties".into(), ListIcon::Glyph(PARTY_ICON, 0xff00ff)),
                ("Platforms".into(), ListIcon::Glyph('\u{f0379}', 0xc0f0c0)),
                ("Categories".into(), ListIcon::Glyph('\u{f03a}', 0xf0a080)),
            ]
            .into(),
        );
    })?;

    navigator.register("*/{id}/dls", |path: &[&str], files: &'static [EmuFile]| {
        let id = path[1].parse::<usize>().unwrap_or(0);
        DownloadSource::new(&files[id])
    })?;

    Ok(())
}

pub fn setup_navigator_bevy(
    settings: Res<AppSettings>,
    playlists: Res<Playlists>,
    mut navigator: ResMut<Navigator>,
) -> Result<()> {
    setup_navigator(&settings.files, &mut navigator)?;
    for list in &playlists.lists {
        navigator.add_playlist(&list.name, list.files);
    }
    navigator.go_root();
    Ok(())
}

pub(crate) fn handle_navigator(
    input: Res<ButtonInput<KeyCode>>,
    mut navigator: ResMut<Navigator>,
    mut reader: MessageReader<FuzzyListSelect>,
    mut load_writer: MessageWriter<LoadFile>,
    mut list_writer: MessageWriter<ShowFuzzyList>,
    mut settings: ResMut<AppSettings>,
    mut playlists: ResMut<Playlists>,
    hud: Res<HudState>,
) {
    navigator.remember_state(&hud);
    if input.just_pressed(KeyCode::ArrowLeft) {
        navigator.back().show(&mut list_writer);
    } else if input.just_pressed(KeyCode::ArrowRight) {
        navigator.forward().show(&mut list_writer);
    }
    if navigator.pos < 0 {
        return;
    }

    let current = &navigator.stack[navigator.pos as usize];
    let id = current.id;
    let source = current.source.clone();
    let prompt = current.prompt.clone();
    let path = current.path.clone();
    for msg in reader.read() {
        if msg.id == id && id == PLAYLIST_MENU {
            let Some((list, file_id)) = path.rsplit_once('/') else {
                continue;
            };
            if msg.item == 0 {
                let file = file_id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| navigator.files.get(list)?.get(i));
                if let (Some(file), Some(index)) = (file, playlists.find(list)) {
                    playlists.toggle(index, file, "");
                    navigator.add_playlist(list, playlists.lists[index].files);
                }
                navigator.back().show(&mut list_writer);
            } else {
                navigator.enter("dls").show(&mut list_writer);
            }
        } else if msg.id == id {
            debug!("Selected {:?}", msg);
            if let Some(emu_file) = msg.emu_file.clone() {
                if msg.alt && navigator.playlists.contains(&path) {
                    navigator
                        .open_playlist_menu(msg.item)
                        .show(&mut list_writer);
                    continue;
                }
                if msg.alt {
                    let id = msg.item;
                    navigator.enter(&format!("{id}/dls")).show(&mut list_writer);
                    continue;
                }
                let ids = source.search(&prompt, usize::MAX);
                if let Some(index) = ids.iter().position(|&i| source.get_item(i) == msg.item) {
                    if let Some(file_index) = source.file_index(ids[index]) {
                        settings.current_game = file_index as isize;
                    }
                    navigator.current_launch = Some(Launch {
                        source: source.clone(),
                        ids,
                        index: index as isize,
                    });
                }
                load_writer.write(LoadFile {
                    emu_file,
                    target_emulator: None,
                });
            } else {
                navigator.enter(&msg.text).show(&mut list_writer);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        emu_file::EmuFile,
        files::{DbFilter, collect_db},
        navigator::{Navigator, setup_navigator},
    };
    use indexmap::IndexMap;
    use std::path::PathBuf;

    #[test]
    fn test_navigator() {
        let filter = DbFilter::default();
        let mut files = vec![];
        let path: PathBuf = "testdata/demos.txt".into();
        let name = collect_db(&path, &filter, &mut files).unwrap();
        assert_eq!(name, "Demozoo");
        let files: &'static [EmuFile] = files.leak();
        let dbs: IndexMap<String, &'static [EmuFile]> = [(name, files)].into();
        let mut navigator = Navigator::new();
        setup_navigator(&dbs, &mut navigator).unwrap();

        navigator.goto("");
        println!(">>Root");
        for line in navigator.get_showing() {
            println!("{line}");
        }
        println!(">>Demozoo");
        navigator.goto("Demozoo");
        for line in navigator.get_showing() {
            println!("{line}");
        }
        println!(">>Demozoo/Parties");
        navigator.goto("Demozoo/Parties");
        for line in navigator.get_showing() {
            println!("{line}");
        }
        println!(">>Compos");
        navigator.goto("Demozoo/Parties/Evoke 2005");
        for line in navigator.get_showing() {
            println!("{line}");
        }
        println!(">>Demo");
        navigator.goto("Demozoo/Parties/Evoke 2005/Demo");
        for line in navigator.get_showing() {
            println!("{line}");
        }
    }

    fn db(text: &'static str) -> &'static [EmuFile] {
        let mut files = vec![];
        crate::files::collect_db_text(text, &DbFilter::default(), &mut files);
        files.leak()
    }

    /// Every db registers under its own name and the root lists them in the
    /// order they were given, each one navigable on its own.
    #[test]
    fn every_db_gets_its_own_root_entry() {
        let dbs: IndexMap<String, &'static [EmuFile]> = [
            (
                "Demozoo".to_string(),
                db("id:1\ttitle:A\tplatform:Amiga\tdownload:http://x/a.zip\n"),
            ),
            (
                "CSDb".to_string(),
                db("id:2\ttitle:B\tplatform:C64\tdownload:http://x/b.zip\n\
                    id:3\ttitle:C\tplatform:C64\tdownload:http://x/c.zip\n"),
            ),
        ]
        .into();
        let mut navigator = Navigator::new();
        setup_navigator(&dbs, &mut navigator).unwrap();

        navigator.goto("");
        assert_eq!(navigator.get_showing(), vec!["Demozoo", "CSDb"]);

        navigator.goto("CSDb/Platforms");
        assert_eq!(navigator.get_showing(), vec!["C64"]);

        navigator.goto("Demozoo/Platforms");
        assert_eq!(navigator.get_showing(), vec!["Amiga"]);
    }

    #[test]
    fn playlist_shows_its_entries_directly() {
        let mut navigator = Navigator::new();
        navigator.add_playlist(
            "Favorites",
            db("id:1\ttitle:A\tauthor:G\tdownload:http://x/a.zip\n"),
        );
        setup_navigator(&IndexMap::new(), &mut navigator).unwrap();

        navigator.goto("");
        assert_eq!(navigator.get_showing(), vec!["Favorites"]);
        navigator.goto("Favorites");
        assert_eq!(navigator.get_showing(), vec!["A / G"]);
    }

    #[test]
    fn playlist_menu_and_refresh() {
        let mut navigator = Navigator::new();
        navigator.add_playlist(
            "Broken",
            db("id:1\ttitle:A\tauthor:G\tdownload:http://x/a.zip\n"),
        );
        setup_navigator(&IndexMap::new(), &mut navigator).unwrap();
        navigator.go_root();
        navigator.goto("Broken");
        navigator.open_playlist_menu(0);
        assert_eq!(navigator.current_path(), "Broken/0");
        assert_eq!(navigator.get_showing()[1], "Run...");
        navigator.enter("dls");
        assert_eq!(navigator.get_showing(), vec!["a.zip"]);

        navigator.back().back();
        navigator.add_playlist("Broken", &[]);
        assert!(navigator.get_showing().is_empty());
        navigator.add_playlist("New", &[]);
        navigator.back();
        assert_eq!(navigator.get_showing(), vec!["Broken", "New"]);
    }
}
