//! The slow parts of getting something on screen — parsing the database,
//! downloading and unpacking a release — on threads of their own.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

use anyhow::{Context, Result, bail};
use database::csdb_latest;
use database::emu_file::{DemoId, EmuFile, FileSource, GameInfo, Override, UrlList};
use database::files::{DbFilter, collect_db};
use database::overrides;
use regex::Regex;
use release_io::release_override::apply_override_meta;
use release_io::workfile::WorkFile;
use retro_core::backend::InputEvent;

pub enum Loaded {
    Db(Result<(String, &'static [EmuFile])>),
    /// A release ready to start, tagged with the load it answers.
    Demo(u32, Result<Demo>),
}

pub struct Demo {
    pub work: WorkFile,
    /// Keys to press, by core frame.
    pub events: Vec<(u32, InputEvent)>,
    /// Title, author and format, a line each; empty for a release without a title.
    pub info: String,
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
    let include = [Regex::new(&format!(
        "^category:({})$",
        csdb_latest::CATEGORIES.join("|")
    ))?];
    let filter = DbFilter {
        include: &include,
        exclude: &[],
    };
    let mut files = vec![];
    let name = collect_db(path, &filter, &mut files)?;
    // Lower rank is better; 0 is unrated.
    files.sort_by_key(|f| {
        let demo = matches!(f.game_info.category, "Demo" | "One-File Demo");
        (f.game_info.rank == 0, f.game_info.rank, !demo)
    });
    Ok((name, files.leak()))
}

/// The csdb entries of `overrides.toml`, read when the first release is loaded.
fn csdb_overrides() -> &'static HashMap<DemoId, Override> {
    static OVERRIDES: OnceLock<HashMap<DemoId, Override>> = OnceLock::new();
    OVERRIDES.get_or_init(|| {
        overrides::load_default_for(Some("csdb"), crate::player::key_by_name).unwrap_or_else(|e| {
            tracing::warn!("{e:#}");
            HashMap::new()
        })
    })
}

fn prepare(file: EmuFile) -> Result<Demo> {
    let over = file.demo_id().and_then(|id| csdb_overrides().get(&id));
    prepare_with(file, over)
}

fn prepare_with(mut file: EmuFile, over: Option<&Override>) -> Result<Demo> {
    let mut meta: HashMap<String, String> = file
        .meta_strings()
        .map(|(k, v)| (k.to_owned(), v.into_owned()))
        .collect();
    if over.is_some_and(|o| o.sfx) {
        meta.insert(release_io::SFX.into(), "true".into());
    }
    if let Some(url) = over.and_then(|o| o.download_url) {
        file.path = FileSource::Url(UrlList::one(url));
    } else if let Some(name) = over.and_then(|o| o.download) {
        file.path.pick_download(name);
    }
    let mut source = file.path.clone();
    let mut work = loop {
        let mut attempt = source.clone();
        let (path, used) = attempt.resolve_with_progress(&|_, _| {})?;
        let err = match unpack_for_c64(path, &meta, over) {
            Ok(work) => break work,
            Err(e) => e,
        };
        // Try the release's other downloads before giving up.
        let FileSource::Url(urls) = &source else {
            return Err(err);
        };
        let rest: Vec<_> = urls.iter().filter(|u| !used.contains(u)).collect();
        if rest.is_empty() || rest.len() == urls.as_slice().len() {
            return Err(err);
        }
        tracing::warn!("{err:#}; trying the next download");
        source = FileSource::Url(UrlList::from(rest));
    };
    let mut events = vec![];
    if let Some(over) = over {
        // Again, so it also beats what `prepare` derived.
        apply_override_meta(&mut work, over);
        events.extend_from_slice(&over.events);
    }
    events.extend(release_io::c64::boot_events(&work));
    let info = info_text(&file);
    Ok(Demo { work, events, info })
}

fn unpack_for_c64(
    path: &std::path::Path,
    meta: &HashMap<String, String>,
    over: Option<&Override>,
) -> Result<WorkFile> {
    let mut work = release_io::unpack_release(path, meta)
        .with_context(|| format!("Could not unpack {}", path.display()))?;
    if let Some(over) = over {
        release_io::apply_override(&mut work, over, &retro_core::system_dir())?;
    }
    if !release_io::c64::prepare(&mut work, false)? {
        bail!("Nothing in this release runs on a C64");
    }
    Ok(work)
}

fn info_text(file: &EmuFile) -> String {
    let GameInfo {
        title,
        group,
        category,
        ..
    } = file.game_info;
    if title.is_empty() {
        return String::new();
    }
    let year = match file.game_info.year() {
        0 => String::new(),
        year => format!(" ({year})"),
    };
    format!("\"{title}\"\n{group}{year}\nC64 {category}")
        .trim_end()
        .to_owned()
}

#[cfg(test)]
#[path = "tests/load_tests.rs"]
mod tests;
