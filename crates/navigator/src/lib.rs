//! The tree the picker browses: databases at the root, and under each one the
//! lists registered for its paths (`Parties/{name}/{compo}`, ...).

use std::collections::{BTreeSet, HashSet};
use std::hash::{BuildHasher, DefaultHasher, Hash, Hasher, RandomState};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use indexmap::{IndexMap, IndexSet};
use tracing::{debug, warn};

use database::csdb_latest;
use database::emu_file::{Award, EmuFile};
use retro_ui::fuzzy_list::{AllWordsSource, FuzzySource, ListIcon, WordsIconSource};

pub mod picker_sources;

pub use picker_sources::{DownloadSource, IconMode, PickerSource};

pub type ListSource = Arc<dyn FuzzySource<EmuFile>>;

pub use database::playlists::FAVORITES;

/// A list to put on screen, as [`Navigator::take_show`] hands it out.
pub struct ShowList {
    pub id: usize,
    pub source: ListSource,
    pub prompt: String,
    /// Source id of the row to highlight, if it is among the results.
    pub selected: Option<usize>,
    /// Scroll offset of the list, in points.
    pub scroll: f32,
    pub title: Vec<String>,
    pub title_select: i32,
}

/// Icons for the  picker
const FAVORITES_ICON: ListIcon = ListIcon::Glyph('', 0xff4040);
const PLAYLIST_ICON: ListIcon = ListIcon::Glyph('󰲹', 0x4080ff);
const DATABASE_ICON: ListIcon = ListIcon::Glyph('󰆼', 0xa0d8ff);
const AUTHOR_ICON: ListIcon = ListIcon::Glyph('', 0x20a0a0);
const SYSTEM_ICON: ListIcon = ListIcon::Glyph('󰒔', 0xa02020);
const NEW_ICON: ListIcon = ListIcon::Glyph('󰎔', 0xe08040);

const PARTY_ICON: char = '󰍖';
const ALL_ICON: ListIcon = ListIcon::Glyph('', 0xffff00);
const PARTIES_ICON: ListIcon = ListIcon::Glyph(PARTY_ICON, 0xff00ff);
const PLATFORMS_ICON: ListIcon = ListIcon::Glyph('󰍹', 0xc0f0c0);
const CATEGORIES_ICON: ListIcon = ListIcon::Glyph('', 0xf0a080);
const AWARDS_ICON: ListIcon = ListIcon::Glyph('', 0xffd700);
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

pub struct NavList {
    pub id: usize,
    /// Filled when the list is first shown.
    source: OnceLock<ListSource>,
    pub path: Vec<String>,
    /// Search text this list was left with, restored when it comes back up.
    pub prompt: String,
    /// Source id of the row this list was left on.
    pub selected: Option<usize>,
    pub scroll: f32,
    /// Rebuilt in its normal order the next time it is shown.
    shuffled: bool,
}

impl NavList {
    fn new(id: usize, source: Option<ListSource>, path: Vec<String>) -> Self {
        Self {
            id,
            source: source.map(OnceLock::from).unwrap_or_default(),
            path,
            prompt: String::new(),
            selected: None,
            scroll: 0.0,
            shuffled: false,
        }
    }

    fn show(&self, source: ListSource, pos: i32, title: Vec<String>) -> ShowList {
        ShowList {
            id: self.id,
            source,
            prompt: self.prompt.clone(),
            selected: self.selected,
            scroll: self.scroll,
            title,
            title_select: pos,
        }
    }
}

/// The list an entry was launched from, in the order it was shown, so
/// NextFile/PrevFile walk it instead of the whole db.
pub struct Launch {
    source: ListSource,
    ids: Vec<usize>,
    pub index: isize,
}

impl Launch {
    /// Move `d` steps and return the entry there, with its index into the db.
    pub fn step(&mut self, d: isize, wrap: bool) -> Option<(EmuFile, Option<usize>)> {
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

/// A list in a random order that holds while it is up.
struct ShuffledSource {
    inner: ListSource,
    state: RandomState,
}

impl FuzzySource<EmuFile> for ShuffledSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let mut ids = self.inner.search(query, usize::MAX);
        ids.sort_by_cached_key(|&id| self.state.hash_one(id));
        ids.truncate(limit);
        ids
    }
    fn get_text(&self, id: usize) -> String {
        self.inner.get_text(id)
    }
    fn get_item(&self, id: usize) -> usize {
        self.inner.get_item(id)
    }
    fn get_lines(&self, id: usize) -> (String, String) {
        self.inner.get_lines(id)
    }
    fn get_info(&self, id: usize) -> String {
        self.inner.get_info(id)
    }
    fn get_data(&self, id: usize) -> Option<EmuFile> {
        self.inner.get_data(id)
    }
    fn file_index(&self, id: usize) -> Option<usize> {
        self.inner.file_index(id)
    }
    fn get_icon(&self, id: usize) -> (Option<ListIcon>, Option<ListIcon>) {
        self.inner.get_icon(id)
    }
    fn can_shuffle(&self) -> bool {
        true
    }
}

pub const PLAYLIST_MENU: usize = 1;
pub const PLAYLIST_ROOT_MENU: usize = 2;

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

pub struct Navigator {
    pub pos: isize,
    showing: isize,
    pub stack: Vec<NavList>,
    pub files: IndexMap<String, &'static [EmuFile]>,
    mapping: Vec<(Option<String>, Vec<Seg>, DbCallback)>,
    pub playlists: HashSet<String>,
    pub current_launch: Option<Launch>,
}

impl Default for Navigator {
    fn default() -> Self {
        Self::new()
    }
}

impl Navigator {
    pub fn new() -> Self {
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
    /// The lowest stack position reachable: the root list is skipped when it
    /// holds a single database.
    fn min_pos(&self) -> isize {
        let single = self.files.len() == 1 && self.stack.len() > 1;
        (single && self.stack[0].path.is_empty()) as isize
    }

    pub fn back(&mut self) -> &mut Self {
        if self.pos > self.min_pos() {
            self.pos -= 1;
        }
        self
    }
    pub fn forward(&mut self) -> &mut Self {
        if self.pos < self.stack.len() as isize - 1 {
            self.pos += 1;
        }
        self
    }

    /// Go to the deepest list in the stack whose path has at most `depth` parts.
    pub fn to_depth(&mut self, depth: usize) -> &mut Self {
        if let Some(i) = self.stack.iter().rposition(|l| l.path.len() <= depth) {
            self.pos = (i as isize).max(self.min_pos());
        }
        self
    }

    pub fn pop(&mut self) -> &mut Self {
        if self.pos > 0 {
            self.stack.truncate(self.pos as usize);
            self.pos = (self.stack.len() as isize) - 1;
        }
        self
    }

    /// Record the search text and selection of the open list, so going back
    /// to this level later restores them.
    pub fn remember_state(&mut self, query: &str, selected: Option<usize>) {
        let list = &mut self.stack[self.pos as usize];
        if list.prompt != query {
            list.prompt = query.into();
        }
        list.selected = selected;
    }

    pub fn remember_scroll(&mut self, scroll: f32) {
        if let Some(list) = self.stack.get_mut(self.pos as usize) {
            list.scroll = scroll;
        }
    }

    /// The current list, if it is not the one last handed out.
    pub fn take_show(&mut self) -> Option<ShowList> {
        if self.pos == self.showing {
            return None;
        }
        self.showing = self.pos;
        if let Some(list) = self.stack.get_mut(self.pos as usize)
            && std::mem::take(&mut list.shuffled)
        {
            list.source = OnceLock::new();
        }
        self.current()
    }

    /// The current list, whether or not it was handed out before.
    pub fn current(&self) -> Option<ShowList> {
        let list = self.stack.get(usize::try_from(self.pos).ok()?)?;
        let title = self.stack.last()?.path.clone();
        Some(list.show(self.list_source(list), self.pos as i32, title))
    }

    /// Make the list `item` was picked from the one [`Launch::step`] walks,
    /// and return the entry's index into its db.
    pub fn launch(&mut self, item: usize) -> Option<usize> {
        let current = self.stack.get(usize::try_from(self.pos).ok()?)?;
        let source = self.list_source(current);
        let ids = source.search(&current.prompt, usize::MAX);
        let index = ids.iter().position(|&i| source.get_item(i) == item)?;
        let file_index = source.file_index(ids[index]);
        self.current_launch = Some(Launch {
            source,
            ids,
            index: index as isize,
        });
        file_index
    }

    /// Put the current list in a random order, launch its first entry and
    /// return it, with its index into its db.
    pub fn shuffle(&mut self) -> Option<(EmuFile, Option<usize>)> {
        let list = self.stack.get(usize::try_from(self.pos).ok()?)?;
        let inner = self.list_source(list);
        if list.id != 0 || !inner.can_shuffle() {
            return None;
        }
        let source: ListSource = Arc::new(ShuffledSource {
            inner,
            state: RandomState::new(),
        });
        let list = &mut self.stack[self.pos as usize];
        list.source = OnceLock::from(source.clone());
        list.shuffled = true;
        let ids = source.search(&list.prompt, usize::MAX);
        let index = ids.iter().position(|&id| source.get_data(id).is_some())?;
        list.selected = Some(ids[index]);
        let file = source.get_data(ids[index])?;
        let file_index = source.file_index(ids[index]);
        self.current_launch = Some(Launch {
            source,
            ids,
            index: index as isize,
        });
        Some((file, file_index))
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

    fn list_source(&self, list: &NavList) -> ListSource {
        list.source
            .get_or_init(|| {
                if list.path.is_empty() {
                    return self.root_source();
                }
                self.source_for(&list.path)
                    .unwrap_or_else(|| Arc::new(AllWordsSource::new(vec![])))
            })
            .clone()
    }

    pub fn go_root(&mut self) -> &mut Self {
        self.stack.clear();
        self.pos = 0;
        self.stack
            .push(NavList::new(0, Some(self.root_source()), vec![]));
        self
    }

    #[cfg(test)]
    fn goto(&mut self, path: &str) -> &mut Self {
        self.goto_path(split_path(path))
    }

    pub fn goto_path(&mut self, path: Vec<String>) -> &mut Self {
        if path.is_empty() {
            return self.go_root();
        }
        debug!("Goto: {path:?}");
        let next = self.stack.get((self.pos + 1) as usize);
        if next.is_some_and(|l| l.id == 0 && l.path == path) {
            return self.forward();
        }
        let Some(source) = self.source_for(&path) else {
            return self;
        };
        // Keep the lists leading here, and stand in for the missing ones.
        let keep = (0..path.len())
            .take_while(|&i| {
                i as isize <= self.pos && self.stack.get(i).is_some_and(|l| l.path == path[..i])
            })
            .count();
        self.stack.truncate(keep);
        for i in keep..path.len() {
            self.stack.push(NavList::new(0, None, path[..i].to_vec()));
        }
        self.stack.push(NavList::new(0, Some(source), path));
        self.pos = self.stack.len() as isize - 1;
        self.showing = -1;
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
        for (only_db, segs, val) in &self.mapping {
            if only_db.as_ref().is_some_and(|d| d != db) {
                continue;
            }
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
    pub fn files_at(&self, path: &str) -> Option<Vec<EmuFile>> {
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
    pub fn files_under(&self, pattern: &str) -> Vec<EmuFile> {
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

    pub fn enter(&mut self, names: &[&str]) -> &mut Self {
        let mut path = self.current_path().to_vec();
        path.extend(names.iter().map(|n| n.to_string()));
        self.goto_path(path)
    }

    #[cfg(test)]
    fn get_showing(&self) -> Vec<String> {
        if self.pos < 0 {
            return vec![];
        }
        self.list_source(&self.stack[self.pos as usize])
            .get_all_strings()
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
            root.source = OnceLock::from(root_source);
        }
        for list in &mut self.stack {
            if list.id == 0 && list.path == [name] {
                let source: ListSource = Arc::new(PickerSource::new(files, None, IconMode::All));
                list.source = OnceLock::from(source);
            }
        }
    }

    pub fn remove_playlist(&mut self, name: &str) {
        self.playlists.remove(name);
        self.files.shift_remove(name);
        let root_source = self.root_source();
        if let Some(root) = self.stack.first_mut() {
            root.source = OnceLock::from(root_source);
        }
    }

    /// Shift+Enter on playlist `name` in the root list.
    pub fn open_playlist_root_menu(&mut self, name: &str) -> &mut Self {
        let source: ListSource = Arc::new(AllWordsSource::new(vec!["󰅖 Delete Playlist".into()]));
        self.push(NavList::new(
            PLAYLIST_ROOT_MENU,
            Some(source),
            vec![name.into()],
        ))
    }

    /// Shift+Enter on entry `id` of a playlist: remove it, or go on to its
    /// downloads.
    pub fn open_playlist_menu(&mut self, id: usize) -> &mut Self {
        let list = self.current_path().join("/");
        let source: ListSource = Arc::new(AllWordsSource::new(vec![
            format!("󰅖 Remove from {list}"),
            "Run...".into(),
        ]));
        self.push(NavList::new(
            PLAYLIST_MENU,
            Some(source),
            vec![list, id.to_string()],
        ))
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
        self.register_in(None, pattern, callback)
    }

    /// Like `register`, but only for the database `db`.
    pub fn register_db<S: FuzzySource<EmuFile>>(
        &mut self,
        db: &str,
        pattern: &str,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> Option<S> + Send + Sync + 'static,
    ) -> Result<()> {
        self.register_in(Some(db.to_string()), pattern, callback)
    }

    fn register_in<S: FuzzySource<EmuFile>>(
        &mut self,
        db: Option<String>,
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
            db,
            segs,
            Box::new(move |groups, files| Some(Arc::new(callback(groups, files)?) as ListSource)),
        ));
        Ok(())
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

fn category_list(files: &'static [EmuFile]) -> WordsIconSource {
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
    WordsIconSource::new(
        cats.into_iter()
            .map(|c| {
                let icon = picker_sources::category_list_icon(&c);
                (c, icon)
            })
            .collect(),
    )
}

fn category_entries(path: &[&str], files: &'static [EmuFile]) -> Option<PickerSource> {
    let subset: Vec<u32> = files
        .iter()
        .enumerate()
        .filter(|(_, f)| *f.categories == [path[1]])
        .map(|(i, _)| i as u32)
        .collect();
    Some(PickerSource::new(files, Some(subset), IconMode::Platforms))
}

/// minimarc's Demozoo root: no Platforms or Awards. Call before
/// [`setup_navigator`], so it is matched first.
pub fn setup_phone(navigator: &mut Navigator) -> Result<()> {
    navigator.register_db("Demozoo", "", |_path: &[&str], _files: &'static [EmuFile]| {
        Some(WordsIconSource::new(vec![
            ("All".into(), ALL_ICON),
            ("Parties".into(), PARTIES_ICON),
            ("Categories".into(), CATEGORIES_ICON),
            ("Authors".into(), AUTHOR_ICON),
        ]))
    })
}

pub fn setup_navigator(
    dbs: &IndexMap<String, &'static [EmuFile]>,
    navigator: &mut Navigator,
    system_dir: &Path,
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
                .flat_map(|f| f.platforms.iter().copied())
                .collect::<IndexSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        ))
    })?;
    navigator.register("Categories", |_path: &[&str], files: &'static [EmuFile]| {
        Some(category_list(files))
    })?;
    navigator.register("Authors", |_path: &[&str], files: &'static [EmuFile]| {
        let authors = files
            .iter()
            .flat_map(|f| f.get_meta("author").split(" & "))
            .filter(|p| !p.is_empty())
            .collect::<IndexSet<_>>()
            .into_iter()
            .map(|a| (a.to_string(), AUTHOR_ICON))
            .collect();
        Some(WordsIconSource::new(authors))
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
            Some(PickerSource::new(files, Some(subset), IconMode::Platforms))
        },
    )?;
    navigator.register(
        "Platforms/{platform}",
        |path: &[&str], files: &'static [EmuFile]| {
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.platforms.contains(&path[1]))
                .map(|(i, _)| i as u32)
                .collect();
            Some(PickerSource::new(
                files,
                Some(subset),
                IconMode::CategoryOnly,
            ))
        },
    )?;

    navigator.register("Categories/{category}", category_entries)?;
    navigator.register(
        "Parties/{name}",
        |path: &[&str], files: &'static [EmuFile]| {
            Some(WordsIconSource::new(
                files
                    .iter()
                    .filter(|f| f.get_party() == path[1])
                    .map(|f| f.get_compo())
                    .filter(|p| !p.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(|c| (c.to_string(), picker_sources::compo_icon(c)))
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

    navigator.register_db("CSDb", "", |_path: &[&str], _files: &'static [EmuFile]| {
        Some(WordsIconSource::new(vec![
            ("All".into(), ALL_ICON),
            ("Parties".into(), PARTIES_ICON),
            ("Categories".into(), CATEGORIES_ICON),
            ("Authors".into(), AUTHOR_ICON),
            ("New Releases".into(), NEW_ICON),
            ("System".into(), SYSTEM_ICON),
        ]))
    })?;
    navigator.register_db(
        "CSDb",
        "New Releases",
        |_path: &[&str], _files: &'static [EmuFile]| {
            let files = csdb_latest::new_releases()
                .inspect_err(|e| warn!("CSDb new releases: {e:#}"))
                .ok()?;
            Some(PickerSource::new(files, None, IconMode::Categories))
        },
    )?;
    navigator.register_db(
        "CSDb",
        "New Releases/{id}/dls",
        |path: &[&str], _files: &'static [EmuFile]| {
            let files = csdb_latest::new_releases().ok()?;
            Some(DownloadSource::new(
                files.get(path[1].parse::<usize>().ok()?)?,
            ))
        },
    )?;

    let p0 = system_dir.to_owned().join("vice/C64/rr38ppal.crt");
    let p1 = system_dir.to_owned().join("vice/C64/tar_v2_pal.crt");
    let b = Box::leak(Box::new([
        EmuFile::new("Retro Replay", p0),
        EmuFile::new("Turbo Action Rom", p1),
    ]));
    navigator.register_db(
        "CSDb",
        "System",
        |_path: &[&str], _files: &'static [EmuFile]| {
            Some(PickerSource::new(b, None, IconMode::Platforms))
        },
    )?;
    navigator.register("", |_path: &[&str], _files: &'static [EmuFile]| {
        Some(WordsIconSource::new(
            [
                ("All".into(), ALL_ICON),
                ("Parties".into(), PARTIES_ICON),
                ("Platforms".into(), PLATFORMS_ICON),
                ("Categories".into(), CATEGORIES_ICON),
                ("Authors".into(), AUTHOR_ICON),
                ("Awards".into(), AWARDS_ICON),
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

#[cfg(test)]
#[path = "tests/navigator_tests.rs"]
mod tests;
