use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::Result;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::emu_file::{EmuFile, FileSource, GameInfo, UrlList};
use crate::files::leak;
use crate::fuzzy_list::{AllWordsSource, FuzzySource};

pub const FAVORITES: &str = "Favorites";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlaylistEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub meta: BTreeMap<String, String>,
}

impl PlaylistEntry {
    /// `fetched` are the `;`-separated URLs the release was loaded from, which
    /// are moved first so loading the entry picks the same download.
    pub fn new(file: &EmuFile, fetched: &str) -> Self {
        let mut entry = Self {
            id: entry_id(file),
            path: match &file.path {
                FileSource::Path(p) => Some(absolute(p)),
                FileSource::Url(_) => None,
            },
            meta: file
                .meta
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        };
        if let Some(download) = entry.meta.get_mut("download") {
            let used: Vec<&str> = fetched.split(';').collect();
            let (mut urls, rest): (Vec<&str>, Vec<&str>) =
                download.split(';').partition(|url| used.contains(url));
            urls.extend(rest);
            *download = urls.join(";");
        }
        entry
    }

    pub fn to_emu_file(&self) -> EmuFile {
        let meta: HashMap<&'static str, &'static str> = self
            .meta
            .iter()
            .map(|(k, v)| (leak(k.clone()), leak(v.clone())))
            .collect();
        let path = match &self.path {
            Some(p) => FileSource::Path(p.into()),
            None => FileSource::Url(UrlList::parse_field(
                meta.get("download").copied().unwrap_or(""),
            )),
        };
        EmuFile {
            game_info: GameInfo::new(&meta),
            path,
            meta,
        }
    }
}

/// `db:id` for a db entry, otherwise wherever the file comes from.
pub fn entry_id(file: &EmuFile) -> String {
    if let Some(id) = file.demo_id() {
        return format!("{}:{}", id.db, id.id);
    }
    match &file.path {
        FileSource::Path(p) => absolute(p),
        FileSource::Url(urls) => urls.first().unwrap_or("").to_owned(),
    }
}

fn absolute(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_owned())
        .to_string_lossy()
        .into_owned()
}

pub struct Playlist {
    pub name: String,
    path: PathBuf,
    entries: Vec<PlaylistEntry>,
    ids: HashSet<String>,
    pub files: &'static [EmuFile],
}

impl Playlist {
    fn new(name: String, path: PathBuf, entries: Vec<PlaylistEntry>) -> Self {
        let mut list = Self {
            name,
            path,
            ids: entries.iter().map(|e| e.id.clone()).collect(),
            entries,
            files: &[],
        };
        list.update_files();
        list
    }

    fn update_files(&mut self) {
        self.files = self
            .entries
            .iter()
            .map(PlaylistEntry::to_emu_file)
            .collect::<Vec<_>>()
            .leak();
    }

    pub fn contains(&self, file: &EmuFile) -> bool {
        self.ids.contains(&entry_id(file))
    }

    /// Add `file`, or remove it if it is already here. Returns whether it is
    /// in the list now.
    fn toggle(&mut self, file: &EmuFile, fetched: &str) -> bool {
        let id = entry_id(file);
        let added = if self.ids.remove(&id) {
            self.entries.retain(|e| e.id != id);
            false
        } else {
            self.ids.insert(id);
            self.entries.push(PlaylistEntry::new(file, fetched));
            true
        };
        self.update_files();
        if let Err(err) = self.save() {
            error!("Can't save {}: {err:#}", self.path.display());
        }
        added
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(&self.entries)?)?;
        Ok(())
    }
}

#[derive(Resource)]
pub struct Playlists {
    /// Favorites first, the rest by name.
    pub lists: Vec<Playlist>,
    dir: PathBuf,
}

pub fn default_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_default()
        .join("demarc")
        .join("playlists")
}

impl Playlists {
    pub fn load(dir: &Path) -> Self {
        let mut lists = vec![];
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        paths.retain(|p| p.extension().is_some_and(|e| e == "json"));
        paths.sort();
        for path in paths {
            let Some(name) = path.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            let entries = fs::read_to_string(&path)
                .map_err(anyhow::Error::from)
                .and_then(|text| Ok(serde_json::from_str::<Vec<PlaylistEntry>>(&text)?));
            match entries {
                Ok(entries) => lists.push(Playlist::new(name, path, entries)),
                Err(err) => warn!("Can't read playlist {}: {err:#}", path.display()),
            }
        }
        if !lists.iter().any(|l| l.name == FAVORITES) {
            lists.push(Playlist::new(
                FAVORITES.into(),
                dir.join(format!("{FAVORITES}.json")),
                vec![],
            ));
        }
        lists.sort_by_key(|l| l.name != FAVORITES);
        Self {
            lists,
            dir: dir.to_owned(),
        }
    }

    pub fn is_favorite(&self, file: &EmuFile) -> bool {
        self.lists[0].contains(file)
    }

    pub fn toggle(&mut self, index: usize, file: &EmuFile, fetched: &str) -> bool {
        self.lists[index].toggle(file, fetched)
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        let name = name.trim().to_lowercase();
        self.lists
            .iter()
            .position(|l| l.name.to_lowercase() == name)
    }

    /// Index of the list called `name`, created empty if there is none yet.
    /// `None` if `name` can't be a file name.
    pub fn create(&mut self, name: &str) -> Option<usize> {
        let name = name.trim();
        if name.is_empty() || name.starts_with('.') || name.contains(['/', '\\']) {
            return None;
        }
        if let Some(index) = self.find(name) {
            return Some(index);
        }
        let path = self.dir.join(format!("{name}.json"));
        let index = 1 + self.lists[1..].partition_point(|l| l.name.as_str() < name);
        self.lists
            .insert(index, Playlist::new(name.into(), path, vec![]));
        Some(index)
    }
}

/// Picks a playlist for `file`: one row per list, ticked if `file` is in it,
/// plus a row to create a new list when the query names none of them.
pub struct PlaylistPicker {
    names: AllWordsSource,
    rows: Vec<String>,
    query: Mutex<String>,
}

impl PlaylistPicker {
    pub fn new(playlists: &Playlists, file: &EmuFile) -> Self {
        let names = playlists.lists.iter().map(|l| l.name.clone()).collect();
        let rows = playlists
            .lists
            .iter()
            .map(|l| {
                if l.contains(file) {
                    format!("{} \u{f012c}", l.name)
                } else {
                    l.name.clone()
                }
            })
            .collect();
        Self {
            names: AllWordsSource::new(names),
            rows,
            query: Mutex::new(String::new()),
        }
    }

    /// The trimmed search text, which is the name a new list gets.
    pub fn query(&self) -> String {
        self.query.lock().unwrap().clone()
    }
}

impl FuzzySource<EmuFile> for PlaylistPicker {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let query = query.trim();
        *self.query.lock().unwrap() = query.to_owned();
        let mut ids = self.names.search(query, limit);
        let lower = query.to_lowercase();
        let exists = (0..self.rows.len()).any(|i| self.names.get_text(i).to_lowercase() == lower);
        if !query.is_empty() && !exists {
            ids.push(self.rows.len());
        }
        ids
    }

    fn get_text(&self, id: usize) -> String {
        match self.rows.get(id) {
            Some(row) => row.clone(),
            None => format!("+ New \"{}\"", self.query()),
        }
    }
}

#[cfg(test)]
#[path = "tests/playlists_tests.rs"]
mod tests;
