//! The slow parts of getting something on screen — parsing the database,
//! downloading and unpacking a release — on threads of their own.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result, bail};
use database::emu_file::EmuFile;
use database::files::{DbFilter, collect_db};
use regex::Regex;
use release_io::workfile::WorkFile;

/// The csdb categories that are something to watch.
const CATEGORIES: &str = "^category:(Demo|One-File Demo|Intro|Crack Intro|4K Intro|1K Intro|\
    256b Intro|Basic Demo|Fake Demo|Invitation|Diskmag|Intro Collection|REU Release|\
    EasyFlash Release)$";

pub enum Loaded {
    Db(Result<(String, &'static [EmuFile])>),
    /// A release ready to start, tagged with the load it answers.
    Demo(u32, Result<WorkFile>),
}

pub struct Loader {
    tx: Sender<Loaded>,
    rx: Receiver<Loaded>,
    /// The most recent demo load; results of older ones are dropped.
    current: u32,
}

impl Loader {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx, current: 0 }
    }

    pub fn load_db(&self, path: PathBuf) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Loaded::Db(read_db(&path)));
        });
    }

    pub fn load_demo(&mut self, file: EmuFile) {
        self.current += 1;
        let (tx, id) = (self.tx.clone(), self.current);
        std::thread::spawn(move || {
            let _ = tx.send(Loaded::Demo(id, prepare(file)));
        });
    }

    pub fn poll(&mut self) -> Option<Loaded> {
        loop {
            match self.rx.try_recv().ok()? {
                Loaded::Demo(id, _) if id != self.current => {}
                loaded => return Some(loaded),
            }
        }
    }
}

fn read_db(path: &std::path::Path) -> Result<(String, &'static [EmuFile])> {
    let include = [Regex::new(CATEGORIES)?];
    let filter = DbFilter {
        include: &include,
        exclude: &[],
    };
    let mut files = vec![];
    let name = collect_db(path, &filter, &mut files)?;
    // Lower rank is better; 0 is unrated.
    files.sort_by_key(|f| (f.game_info.rank == 0, f.game_info.rank));
    Ok((name, files.leak()))
}

fn prepare(mut file: EmuFile) -> Result<WorkFile> {
    let meta: HashMap<String, String> = file
        .meta_strings()
        .map(|(k, v)| (k.to_owned(), v.into_owned()))
        .collect();
    let (path, _) = file.path.resolve_with_progress(&|_, _| {})?;
    let mut work = release_io::unpack_release(path, &meta)
        .with_context(|| format!("Could not unpack {}", path.display()))?;
    if !release_io::c64::prepare(&mut work, false)? {
        bail!("Nothing in this release runs on a C64");
    }
    Ok(work)
}

#[cfg(test)]
#[path = "tests/load_tests.rs"]
mod tests;
