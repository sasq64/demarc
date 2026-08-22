//! The set of demos the user has starred, and the file it lives in.
//!
//! A favorite is a [`DbId`] — a db and an id within it — so the same number in
//! two different databases stays two different demos. Files off disk have no
//! id and so can't be favorited; there is nothing stable to remember them by.
//!
//! The set is behind a lock rather than owned by whoever mutates it, because
//! the file picker's search index is built once and reused ([`AppSettings`] in
//! `crate::main`). A toggle has to be visible through the [`Arc`] the picker
//! already holds, without rebuilding that index.
//!
//! [`Arc`]: std::sync::Arc
//! [`AppSettings`]: crate::main

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};

use tracing::warn;

use crate::emu_file::DbId;

/// The starred entries, and where they are stored between runs.
#[derive(Default)]
pub struct Favorites {
    ids: RwLock<HashSet<DbId>>,
    /// `None` when there is no config directory to write to. The set still
    /// works for the rest of the run, it just doesn't outlive it — favoriting
    /// silently doing nothing would be worse than favoriting not persisting.
    path: Option<PathBuf>,
}

impl Favorites {
    /// The user's favorites, read from `<config>/demarc/favorites`.
    ///
    /// A missing file is simply an empty set — the first favorite creates it.
    pub fn load() -> Self {
        let Some(path) = dirs::config_dir().map(|d| d.join("demarc").join("favorites")) else {
            warn!("No config directory; favorites will not be saved");
            return Self::default();
        };
        Self::read(path)
    }

    /// The favorites stored at `path`, whether or not the file is there yet.
    fn read(path: PathBuf) -> Self {
        let ids = match fs::read_to_string(&path) {
            Ok(text) => parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(err) => {
                warn!("Failed to read favorites from {}: {err}", path.display());
                HashSet::new()
            }
        };
        Self {
            ids: RwLock::new(ids),
            path: Some(path),
        }
    }

    /// Whether `id` is starred.
    pub fn contains(&self, id: &DbId) -> bool {
        self.ids.read().unwrap().contains(id)
    }

    /// Star `id` if it isn't, unstar it if it is, and write the set back out.
    /// Returns whether it is a favorite now, for the message shown to the user.
    pub fn toggle(&self, id: &DbId) -> bool {
        let now = {
            let mut ids = self.ids.write().unwrap();
            if ids.remove(id) {
                false
            } else {
                ids.insert(id.clone());
                true
            }
        };
        self.save();
        now
    }

    /// Write the set out, one `source:id` per line, sorted so the file doesn't
    /// churn between runs. A failure is reported and otherwise ignored: losing
    /// a favorite is not worth taking the emulator down for.
    fn save(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let mut lines: Vec<String> = self.ids.read().unwrap().iter().map(DbId::to_string).collect();
        lines.sort();
        lines.push(String::new()); // trailing newline
        if let Err(err) = write_all(path, &lines.join("\n")) {
            warn!("Failed to save favorites to {}: {err}", path.display());
        }
    }
}

fn write_all(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)
}

/// Read the favorites file: one `source:id` per line, with blank lines and `#`
/// comments skipped. A line that doesn't parse is warned about and dropped
/// rather than throwing away the rest of the file with it.
fn parse(text: &str) -> HashSet<DbId> {
    let mut ids = HashSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line.parse::<DbId>() {
            Ok(id) => {
                ids.insert(id);
            }
            Err(err) => warn!("Skipping unreadable favorite {line:?}: {err}"),
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emu_file::DbSource;

    fn id(source: DbSource, id: u32) -> DbId {
        DbId { source, id }
    }

    /// Toggling writes through to the file, and a fresh `Favorites` over the
    /// same path sees exactly what was left there.
    #[test]
    fn favorites_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        // Under a directory that doesn't exist yet, like the real config dir on
        // a first run.
        let path = dir.path().join("demarc").join("favorites");

        let favorites = Favorites::read(path.clone());
        let eod = id(DbSource::Csdb, 1);
        let victrip = id(DbSource::Demozoo, 10);

        assert!(!favorites.contains(&eod), "nothing is starred to begin with");
        assert!(favorites.toggle(&eod), "toggling on reports the new state");
        assert!(favorites.toggle(&victrip));
        assert!(favorites.contains(&eod));

        // Sorted, one per line, in the form `DbId` parses back.
        assert_eq!(fs::read_to_string(&path).unwrap(), "csdb:1\ndemozoo:10\n");

        let reloaded = Favorites::read(path.clone());
        assert!(reloaded.contains(&eod));
        assert!(reloaded.contains(&victrip));
        // An id is only a favorite in the db it came from.
        assert!(!reloaded.contains(&id(DbSource::Demozoo, 1)));

        assert!(!reloaded.toggle(&eod), "toggling off reports the new state");
        assert_eq!(fs::read_to_string(&path).unwrap(), "demozoo:10\n");
        assert!(!Favorites::read(path).contains(&eod));
    }

    /// A hand-edited file keeps working: comments and blanks are ignored, and
    /// one bad line doesn't cost the user the rest of their favorites.
    #[test]
    fn unreadable_lines_are_skipped() {
        let ids = parse("# mine\n\ncsdb:1\nnot an id\ndemozoo:10\n");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id(DbSource::Csdb, 1)));
        assert!(ids.contains(&id(DbSource::Demozoo, 10)));
    }

    /// With nowhere to write, favoriting still works for the run rather than
    /// failing outright.
    #[test]
    fn a_favorites_set_with_no_file_still_works() {
        let favorites = Favorites::default();
        let eod = id(DbSource::Csdb, 1);
        assert!(favorites.toggle(&eod));
        assert!(favorites.contains(&eod));
    }
}
