use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use crate::emu_file::{EmuFile, FileSource, GameInfo, UrlList};
use crate::files::leak;

pub const FAVORITES: &str = "Favorites";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlaylistEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub meta: BTreeMap<String, String>,
}

impl PlaylistEntry {
    pub fn new(file: &EmuFile) -> Self {
        Self {
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
        }
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
    pub files: &'static [EmuFile],
}

impl Playlist {
    fn new(name: String, path: PathBuf, entries: Vec<PlaylistEntry>) -> Self {
        let mut list = Self {
            name,
            path,
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
    favorite_ids: HashSet<String>,
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
        let favorite_ids = lists[0].entries.iter().map(|e| e.id.clone()).collect();
        Self {
            lists,
            favorite_ids,
        }
    }

    pub fn favorites(&self) -> &Playlist {
        &self.lists[0]
    }

    pub fn is_favorite(&self, file: &EmuFile) -> bool {
        self.favorite_ids.contains(&entry_id(file))
    }

    /// Add `file` to Favorites, or remove it if it is already there. Returns
    /// whether it is a favorite now.
    pub fn toggle_favorite(&mut self, file: &EmuFile) -> bool {
        let id = entry_id(file);
        let list = &mut self.lists[0];
        let added = if self.favorite_ids.remove(&id) {
            list.entries.retain(|e| e.id != id);
            false
        } else {
            self.favorite_ids.insert(id);
            list.entries.push(PlaylistEntry::new(file));
            true
        };
        list.update_files();
        if let Err(err) = list.save() {
            error!("Can't save {}: {err:#}", list.path.display());
        }
        added
    }
}

#[cfg(test)]
#[path = "tests/playlists_tests.rs"]
mod tests;
