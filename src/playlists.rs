use std::path::Path;
use std::sync::Mutex;

use bevy::prelude::*;

use crate::emu_file::EmuFile;
use crate::fuzzy_list::{AllWordsSource, FuzzySource};

pub use ::database::playlists::{FAVORITES, default_dir, entry_id};

#[derive(Resource, Deref, DerefMut)]
pub struct Playlists(pub ::database::playlists::Playlists);

impl Playlists {
    pub fn load(dir: &Path) -> Self {
        Self(::database::playlists::Playlists::load(dir))
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
