use std::collections::{BTreeSet, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use bevy::prelude::*;
use indexmap::{IndexMap, IndexSet};

use crate::AppState;
use crate::config::AppSettings;
use crate::egui_ui::HudState;
use crate::emu_file::{Award, EmuFile};
use crate::fuzzy_list::{AllWordsSource, FuzzySource, ListIcon, WordsIconSource};
use crate::loading::LoadFile;
use crate::picker_sources::{DownloadSource, IconMode, PickerSource};
use crate::playlists::{FAVORITES, Playlists};
use crate::ui::{FuzzyListSelect, ListSource, ShowFuzzyList};
use retro_ui::TitleClick;

/// Icons for the root list, as nerd font glyphs: the favorites playlist, the
/// other playlists, and the databases.
const FAVORITES_ICON: ListIcon = ListIcon::Glyph('', 0xff4040);
const PLAYLIST_ICON: ListIcon = ListIcon::Glyph('󰲹', 0x4080ff);
const DATABASE_ICON: ListIcon = ListIcon::Glyph('󰆼', 0xa0d8ff);

const PARTY_ICON: char = '󰍖';
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
    path: Vec<String>,
    /// Search text this list was left with, restored when it comes back up.
    prompt: String,
    /// Source id of the row this list was left on.
    selected: Option<usize>,
}

impl NavList {
    pub(crate) fn show(
        &self,
        pos: i32,
        title: Vec<String>,
        show_list: &mut MessageWriter<ShowFuzzyList>,
    ) {
        show_list.write(ShowFuzzyList {
            id: self.id,
            source: self.source.clone(),
            prompt: Some(self.prompt.clone()),
            selected: self.selected,
            title,
            title_select: pos,
            keep_open: true,
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
const PLAYLIST_ROOT_MENU: usize = 2;

type DbCallback = Box<dyn Fn(&[&str], &'static [EmuFile]) -> Option<ListSource> + Send + Sync>;

enum Seg {
    Lit(String),
    Capture,
    /// Any number of path parts.
    Any,
}

/// The captures of `segs` matched against `path`, or None if it doesn't match.
fn match_segs<'a>(segs: &[Seg], path: &'a [String], out: &mut Vec<&'a str>) -> bool {
    match segs.split_first() {
        None => path.is_empty(),
        Some((Seg::Any, rest)) => (0..=path.len()).any(|skip| {
            let len = out.len();
            match_segs(rest, &path[skip..], out) || {
                out.truncate(len);
                false
            }
        }),
        Some((seg, rest)) => {
            let Some((first, path)) = path.split_first() else {
                return false;
            };
            match seg {
                Seg::Lit(lit) if lit != first => return false,
                Seg::Capture => out.push(first),
                _ => {}
            }
            match_segs(rest, path, out)
        }
    }
}

#[derive(Resource)]
pub(crate) struct Navigator {
    pub(crate) pos: isize,
    showing: isize,
    pub(crate) stack: Vec<NavList>,
    files: IndexMap<String, &'static [EmuFile]>,
    mapping: Vec<(Vec<Seg>, DbCallback)>,
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

    /// Go to the deepest list in the stack whose path has at most `depth` parts.
    fn to_depth(&mut self, depth: usize) -> &mut Self {
        if let Some(i) = self.stack.iter().rposition(|l| l.path.len() <= depth) {
            self.pos = i as isize;
        }
        self
    }

    fn pop(&mut self) -> &mut Self {
        if self.pos > 0 {
            self.stack.truncate(self.pos as usize);
            self.pos = (self.stack.len() as isize) - 1;
        }
        self
    }

    /// Take the search text and selection out of the open list, so going back
    /// to this level later restores them.
    fn remember_state(&mut self, hud: &HudState) {
        let list = &mut self.stack[self.pos as usize];
        if let Some(query) = hud.list_query(list.id) {
            if list.prompt != query {
                list.prompt = query.into();
            }
            list.selected = hud.list_selected_item(list.id);
        }
    }

    pub(crate) fn show(&mut self, lw: &mut MessageWriter<ShowFuzzyList>) {
        if self.pos != self.showing {
            let title = self.stack.last().unwrap().path.clone();
            self.stack[self.pos as usize].show(self.pos as i32, title, lw);
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
            path: vec![],
            prompt: String::new(),
            selected: None,
        });
        self
    }

    #[cfg(test)]
    fn goto(&mut self, path: &str) -> &mut Self {
        self.goto_path(split_path(path))
    }

    fn goto_path(&mut self, path: Vec<String>) -> &mut Self {
        if path.is_empty() {
            return self.go_root();
        }
        debug!("Goto: {path:?}");
        if let Some(source) = self.source_for(&path) {
            self.push(NavList {
                id: 0,
                source,
                path,
                prompt: String::new(),
                selected: None,
            });
        }
        self
    }

    fn source_for(&self, path: &[String]) -> Option<ListSource> {
        let (db, rest) = path.split_first()?;
        let Some(files) = self.files.get(db).copied() else {
            warn!("No such database: {db}");
            return None;
        };
        if rest.is_empty() && self.playlists.contains(db) {
            return Some(Arc::new(PickerSource::new(files, None, IconMode::All)));
        }
        let joined = rest.join("/");
        for (segs, val) in &self.mapping {
            let mut groups = vec![joined.as_str()];
            // A callback returning None declines the path; keep looking.
            if match_segs(segs, rest, &mut groups)
                && let Some(source) = val(&groups, files)
            {
                return Some(source);
            }
        }
        None
    }

    /// The entries listed at `path`, in the order shown. A path that lists
    /// folders has none; `None` if the path doesn't exist.
    pub(crate) fn files_at(&self, path: &str) -> Option<Vec<EmuFile>> {
        let source = self.source_for(&split_path(path))?;
        let ids = source.search("", usize::MAX);
        Some(
            ids.into_iter()
                .filter_map(|id| source.get_data(id))
                .collect(),
        )
    }

    /// Every entry in every folder below the paths `pattern` matches. A `*` in
    /// a path segment matches any text.
    pub(crate) fn files_under(&self, pattern: &str) -> Vec<EmuFile> {
        let mut files = vec![];
        let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
        self.collect_files(&[], &segments, &mut files);
        files
    }

    fn collect_files(&self, path: &[String], pattern: &[&str], out: &mut Vec<EmuFile>) {
        let join = |name: &str| [path, &[name.to_string()]].concat();
        if let Some((first, rest)) = pattern.split_first()
            && !first.contains('*')
        {
            return self.collect_files(&join(first), rest, out);
        }
        let source = if path.is_empty() {
            Some(self.root_source())
        } else {
            self.source_for(path)
        };
        let Some(source) = source else { return };
        let ids = source.search("", usize::MAX);
        if pattern.is_empty() {
            let files: Vec<_> = ids.iter().filter_map(|&id| source.get_data(id)).collect();
            if !files.is_empty() {
                out.extend(files);
                return;
            }
        }
        let (want, rest) = pattern
            .split_first()
            .map_or(("*", &[][..]), |(w, r)| (*w, r));
        for id in ids {
            let name = source.get_text(id);
            if source.get_data(id).is_none() && glob_match(want, &name) {
                self.collect_files(&join(&name), rest, out);
            }
        }
    }

    fn current_path(&self) -> &[String] {
        if self.pos < 0 {
            return &[];
        }
        &self.stack[self.pos as usize].path
    }

    fn enter(&mut self, names: &[&str]) -> &mut Self {
        let mut path = self.current_path().to_vec();
        path.extend(names.iter().map(|n| n.to_string()));
        self.goto_path(path)
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
            if list.id == 0 && list.path == [name] {
                list.source = Arc::new(PickerSource::new(files, None, IconMode::All));
            }
        }
    }

    pub fn remove_playlist(&mut self, name: &str) {
        self.playlists.remove(name);
        self.files.shift_remove(name);
        let root_source = self.root_source();
        if let Some(root) = self.stack.first_mut() {
            root.source = root_source;
        }
    }

    /// Shift+Enter on playlist `name` in the root list.
    fn open_playlist_root_menu(&mut self, name: &str) -> &mut Self {
        self.push(NavList {
            id: PLAYLIST_ROOT_MENU,
            source: Arc::new(AllWordsSource::new(vec![
                "\u{f0156} Delete Playlist".into(),
            ])),
            path: vec![name.into()],
            prompt: String::new(),
            selected: None,
        })
    }

    /// Shift+Enter on entry `id` of a playlist: remove it, or go on to its
    /// downloads.
    fn open_playlist_menu(&mut self, id: usize) -> &mut Self {
        let list = self.current_path().join("/");
        self.push(NavList {
            id: PLAYLIST_MENU,
            source: Arc::new(AllWordsSource::new(vec![
                format!("\u{f0156} Remove from {list}"),
                "Run...".into(),
            ])),
            path: vec![list, id.to_string()],
            prompt: String::new(),
            selected: None,
        })
    }

    // Register a path pattern; "Parties/{party}/{combo}" calls `callback` for
    // "Demozoo/Parties/Revision 2022/PC Demo" with ["Revision 2022/PC Demo",
    // "Revision 2022", "PC Demo"] and the Demozoo EmuFiles. A "*" part matches
    // any number of path parts, so "*/{id}/dls" also matches "Platforms/C64/12345/dls".
    // Returning None from the callback declines the path, so the next matching
    // registration is tried instead.
    pub fn register<S: FuzzySource<EmuFile>>(
        &mut self,
        pattern: &str,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> Option<S> + Send + Sync + 'static,
    ) -> Result<()> {
        let segs = split_path(pattern)
            .into_iter()
            .map(|part| match part.as_str() {
                "*" => Seg::Any,
                p if p.starts_with('{') && p.ends_with('}') => Seg::Capture,
                _ => Seg::Lit(part),
            })
            .collect();
        self.mapping.push((
            segs,
            Box::new(move |groups, files| Some(Arc::new(callback(groups, files)?) as ListSource)),
        ));
        Ok(())
    }

    pub fn open(&mut self, show_list: &mut MessageWriter<ShowFuzzyList>) {
        if self.pos >= 0 {
            let title = self.stack.last().unwrap().path.clone();
            self.stack[self.pos as usize].show(self.pos as i32, title, show_list);
        }
    }
}

fn split_path(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
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

fn glob_match(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or("");
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last)
}

/// Awards an entry won or was nominated for, each with the event it was given
/// at. Awards are handed out the year after the release.
fn awards(f: &EmuFile) -> impl Iterator<Item = (String, Award, bool)> {
    let year = match f.game_info.year() {
        0 => f
            .get_party()
            .rsplit(' ')
            .next()
            .and_then(|y| y.parse::<u32>().ok()),
        y => Some(y),
    };
    let wins = f.get_wins().into_iter().map(|a| (a, true));
    let nominees = f.get_nominees().into_iter().map(|a| (a, false));
    wins.chain(nominees).map(move |(award, win)| {
        let event = match (award.series(), year) {
            ((series, true), Some(year)) => format!("{series} {}", year + 1),
            ((series, _), _) => series.to_string(),
        };
        (event, award, win)
    })
}

pub fn setup_navigator(
    dbs: &IndexMap<String, &'static [EmuFile]>,
    navigator: &mut Navigator,
) -> Result<()> {
    for (name, files) in dbs {
        navigator.add_db(name, files);
    }

    navigator.register("All", |_path: &[&str], files: &'static [EmuFile]| {
        Some(PickerSource::new(files, None, IconMode::Categories))
    })?;
    navigator.register("Parties", |_path: &[&str], files: &'static [EmuFile]| {
        Some(WordsIconSource::new(
            files
                .iter()
                .map(|f| f.get_party())
                .filter(|p| !p.is_empty())
                .collect::<IndexSet<_>>()
                .into_iter()
                .map(|p| (p.to_string(), party_icon(p)))
                .collect(),
        ))
    })?;
    navigator.register("Platforms", |_path: &[&str], files: &'static [EmuFile]| {
        Some(AllWordsSource::new(
            files
                .iter()
                .filter_map(|f| match *f.platforms {
                    [one] => Some(one),
                    _ => None,
                })
                .collect::<IndexSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        ))
    })?;
    navigator.register("Categories", |_path: &[&str], files: &'static [EmuFile]| {
        let mut cats: Vec<String> = files
            .iter()
            .filter_map(|f| match *f.categories {
                [one] => Some(one),
                _ => None,
            })
            .collect::<IndexSet<_>>()
            .into_iter()
            .map(String::from)
            .collect();
        // Known categories in CATS order, the rest after in the order they appeared.
        cats.sort_by_key(|c| CATS.iter().position(|n| n == c).unwrap_or(CATS.len()));
        Some(AllWordsSource::new(cats))
    })?;
    navigator.register("Authors", |_path: &[&str], files: &'static [EmuFile]| {
        let authors: Vec<String> = files
            .iter()
            .map(|f| f.get_meta("author"))
            .filter(|p| !p.is_empty() && !p.contains(" & "))
            .collect::<IndexSet<_>>()
            .into_iter()
            .map(String::from)
            .collect();
        Some(AllWordsSource::new(authors))
    })?;
    navigator.register(
        "Authors/{author}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.get_meta("author").split(" & ").any(|s| s == path[1]))
                .map(|(i, _)| i as u32)
                .collect();
            Some(PickerSource::new(
                files,
                Some(subset),
                IconMode::PlatformOnly,
            ))
        },
    )?;
    navigator.register(
        "Platforms/{platform}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| *f.platforms == [path[1]])
                .map(|(i, _)| i as u32)
                .collect();
            Some(PickerSource::new(
                files,
                Some(subset),
                IconMode::CategoryOnly,
            ))
        },
    )?;

    navigator.register(
        "Categories/{category}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| *f.categories == [path[1]])
                .map(|(i, _)| i as u32)
                .collect();
            Some(PickerSource::new(
                files,
                Some(subset),
                IconMode::PlatformOnly,
            ))
        },
    )?;
    navigator.register(
        "Parties/{name}",
        |path: &[&str], files: &'static [EmuFile]| {
            println!("PATH: {}", path[1]);
            Some(AllWordsSource::new(
                files
                    .iter()
                    .filter(|f| f.get_party() == path[1])
                    .map(|f| f.get_compo())
                    .filter(|p| !p.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(String::from)
                    .collect(),
            ))
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
            Some(PickerSource::new(files, Some(subset), IconMode::Platforms))
        },
    )?;

    navigator.register("Awards", |_path: &[&str], files: &'static [EmuFile]| {
        Some(AllWordsSource::new(
            files
                .iter()
                .flat_map(awards)
                .map(|(event, _, _)| event)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
        ))
    })?;
    navigator.register(
        "Awards/{event}",
        |path: &[&str], files: &'static [EmuFile]| {
            Some(AllWordsSource::new(
                files
                    .iter()
                    .flat_map(awards)
                    .filter(|(event, _, _)| event == path[1])
                    .map(|(_, award, _)| award)
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(|a| a.to_string())
                    .collect(),
            ))
        },
    )?;
    navigator.register(
        "Awards/{event}/{award}",
        |path: &[&str], files: &'static [EmuFile]| {
            let mut subset: Vec<(u32, bool)> = files
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    awards(f)
                        .find(|(event, award, _)| event == path[1] && award.to_string() == path[2])
                        .map(|(_, _, win)| (i as u32, win))
                })
                .collect();
            subset.sort_by_key(|(_, win)| !win);
            Some(PickerSource::new(
                files,
                Some(subset.into_iter().map(|(i, _)| i).collect()),
                IconMode::Platforms,
            ))
        },
    )?;

    navigator.register("", |_path: &[&str], _files: &'static [EmuFile]| {
        Some(WordsIconSource::new(
            [
                ("All".into(), ListIcon::Glyph('', 0xffff00)),
                ("Parties".into(), ListIcon::Glyph(PARTY_ICON, 0xff00ff)),
                ("Platforms".into(), ListIcon::Glyph('󰍹', 0xc0f0c0)),
                ("Categories".into(), ListIcon::Glyph('', 0xf0a080)),
                ("Authors".into(), ListIcon::Glyph('', 0x20a0a0)),
                ("Awards".into(), ListIcon::Glyph('\u{f091}', 0xffd700)),
            ]
            .into(),
        ))
    })?;

    navigator.register("*/{id}/dls", |path: &[&str], files: &'static [EmuFile]| {
        if let Some(file) = path[1].parse::<usize>().ok().and_then(|id| files.get(id)) {
            return Some(DownloadSource::new(file));
        }
        None
    })?;

    Ok(())
}

fn setup_navigator_bevy(
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

fn update_navigator(
    input: Res<ButtonInput<KeyCode>>,
    mut navigator: ResMut<Navigator>,
    mut list_writer: MessageWriter<ShowFuzzyList>,
    mut hud: ResMut<HudState>,
) {
    navigator.remember_state(&hud);
    let id = navigator.stack[navigator.pos as usize].id;
    if hud.list_query(id).is_none() {
        return;
    }
    if let Some(click) = hud.take_title_click() {
        match click {
            TitleClick::Depth(depth) => navigator.to_depth(depth),
            TitleClick::Back => navigator.back(),
            TitleClick::Forward => navigator.forward(),
        }
        .show(&mut list_writer);
    } else if input.just_pressed(KeyCode::ArrowLeft) {
        navigator.back().show(&mut list_writer);
    } else if input.just_pressed(KeyCode::ArrowRight) {
        navigator.forward().show(&mut list_writer);
    } else if input.just_pressed(KeyCode::F11) {
        if navigator.pos > 0 {
            navigator.pop();
        }
        //navigator.get_selected();
    }
}

fn handle_selection(
    mut navigator: ResMut<Navigator>,
    mut reader: MessageReader<FuzzyListSelect>,
    mut load_writer: MessageWriter<LoadFile>,
    mut list_writer: MessageWriter<ShowFuzzyList>,
    mut settings: ResMut<AppSettings>,
    mut playlists: ResMut<Playlists>,
    mut hud: ResMut<HudState>,
) {
    let current = &navigator.stack[navigator.pos as usize];
    let id = current.id;
    let source = current.source.clone();
    let prompt = current.prompt.clone();
    let path = current.path.clone();
    for msg in reader.read() {
        if msg.id == id && id == PLAYLIST_ROOT_MENU {
            let name = path.join("/");
            if let Some(index) = playlists.find(&name)
                && playlists.delete(index)
            {
                navigator.remove_playlist(&name);
            }
            navigator.pop().show(&mut list_writer);
        } else if msg.id == id && id == PLAYLIST_MENU {
            let [list, file_id] = path.as_slice() else {
                continue;
            };
            if msg.item == 0 {
                let file = file_id
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| navigator.files.get(list.as_str())?.get(i));
                if let (Some(file), Some(index)) = (file, playlists.find(list)) {
                    playlists.toggle(index, file, "");
                    navigator.add_playlist(list, playlists.lists[index].files);
                }
                navigator.back().show(&mut list_writer);
            } else {
                navigator.enter(&["dls"]).show(&mut list_writer);
            }
        } else if msg.id == id {
            debug!("Selected {:?}", msg);
            if let Some(emu_file) = msg.emu_file.clone() {
                if msg.alt && path.len() == 1 && navigator.playlists.contains(&path[0]) {
                    navigator
                        .open_playlist_menu(msg.item)
                        .show(&mut list_writer);
                    continue;
                }
                if msg.alt {
                    let id = msg.item;
                    navigator
                        .enter(&[&id.to_string(), "dls"])
                        .show(&mut list_writer);
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
                hud.close_list();
                load_writer.write(LoadFile {
                    emu_file,
                    target_emulator: None,
                });
            } else if msg.alt
                && path.is_empty()
                && msg.text != FAVORITES
                && navigator.playlists.contains(&msg.text)
            {
                navigator
                    .open_playlist_root_menu(&msg.text)
                    .show(&mut list_writer);
            } else {
                navigator.enter(&[&msg.text]).show(&mut list_writer);
            }
        }
    }
}

/// When `--select` is passed, open the file-open selector once we start running.
fn open_select_menu(
    args: Res<crate::Args>,
    mut navigator: ResMut<Navigator>,
    mut writer: MessageWriter<ShowFuzzyList>,
) {
    if args.select {
        navigator.show(&mut writer);
    }
}

pub struct NavigatorPlugin;

impl Plugin for NavigatorPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Navigator::new())
            .add_systems(Startup, setup_navigator_bevy)
            .add_systems(OnEnter(AppState::Running), open_select_menu)
            .add_systems(
                Update,
                (
                    update_navigator,
                    handle_selection.run_if(on_message::<FuzzyListSelect>),
                ),
            );
    }
}

#[cfg(test)]
#[path = "tests/navigator_tests.rs"]
mod tests;
