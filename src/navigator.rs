use std::collections::BTreeSet;
use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::*;
use regex::Regex;

use crate::commands::{Cmd, CmdMessage, FilePickerSource, PickerSource};
use crate::config::AppSettings;
use crate::egui_ui::{FuzzyListSelect, ListSource, ShowFuzzyList};
use crate::emu_file::EmuFile;
use crate::fuzzy_list::{AllWordsSource, FuzzySource};

enum ListAction {
    OpenFile,
}

pub(crate) struct NavList {
    id: usize,
    source: ListSource,
}

impl NavList {
    pub(crate) fn show(&self, show_list: &mut MessageWriter<ShowFuzzyList>) {
        show_list.write(ShowFuzzyList {
            id: self.id,
            source: self.source.clone(),
            prompt: Some("".into()),
        });
    }

    // fn getSource(&self) -> ListSource<EmuFile> {}
}

struct RootSource {
    names: AllWordsSource,
}

impl RootSource {
    pub(crate) fn new() -> Self {
        Self {
            names: AllWordsSource::new(vec![
                "Demozoo".into(),
                "CSDb".into(),
                "Parties".into(),
                "Favorites".into(),
            ]),
        }
    }
}

impl FuzzySource<EmuFile> for RootSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, _id: usize) -> String {
        "".into()
    }
}

type DbCallback = Box<dyn Fn(&[&str], &'static [EmuFile]) -> ListSource + Send + Sync>;

#[derive(Resource)]
pub(crate) struct Navigator {
    pub(crate) pos: isize,
    showing: isize,
    pub(crate) stack: Vec<NavList>,
    files: HashMap<&'static str, &'static [EmuFile]>,
    mapping: Vec<(Regex, DbCallback)>,
    path: String,
}

impl Navigator {
    pub(crate) fn new() -> Self {
        let source = Arc::new(RootSource::new());
        let root = NavList { id: 0, source };
        Self {
            pos: -1,
            showing: -1,
            files: HashMap::new(),
            stack: vec![root],
            mapping: Vec::new(),
            path: "".into(),
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

    pub(crate) fn show(&mut self, lw: &mut MessageWriter<ShowFuzzyList>) {
        if self.pos != self.showing {
            self.stack[self.pos as usize].show(lw);
            self.showing = self.pos;
        }
    }

    fn go_root(&mut self) -> &mut Self {
        self.stack.clear();
        self.pos = 0;
        self.stack.push(NavList {
            id: 0,
            source: Arc::new(AllWordsSource::new(["Demozoo".to_string()].into())),
        });
        self
    }

    fn goto(&mut self, path: &str) -> &mut Self {
        if path.is_empty() {
            self.go_root();
            self.path = "".into();
            return self;
        }
        debug!("Goto: '{}'", path);
        let (db, rest) = path.split_once('/').unwrap_or((path, ""));
        let Some(files) = self.files.get(db).copied() else {
            warn!("No such database: {db}");
            return self;
        };
        for (key, val) in &self.mapping {
            if let Some(m) = key.captures(rest) {
                let groups: Vec<&str> = m.iter().map(|m| m.map_or("", |m| m.as_str())).collect();
                let source = val(&groups, files);
                self.push(NavList { id: 0, source });
                self.path = path.into();
                return self;
            }
        }
        self
    }

    fn enter(&mut self, path: &str) -> &mut Self {
        if self.path.is_empty() {
            return self.goto(path);
        }
        self.goto(&(self.path.clone() + "/" + path))
    }

    fn get_showing(&self) -> Vec<String> {
        if self.pos < 0 {
            return vec![];
        }
        self.stack[self.pos as usize].source.get_all_strings()
    }

    /// Add a top level entry to the Navigator. It must be backed by a static
    /// list of EmuFiles that other Navigator parts filter from
    pub fn add_db(&mut self, name: &'static str, files: &'static [EmuFile]) {
        self.files.insert(name, files);
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

    // Register with simpler pattern; "Parties/{party}/{combo}" should become "Parties\/([^\/]*)\/([^\/]*)"
    pub fn register<S: FuzzySource<EmuFile>>(
        &mut self,
        pattern: &str,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> S + Send + Sync + 'static,
    ) -> Result<()> {
        let mut rx = String::from("^.*");
        for (i, part) in pattern.split('/').enumerate() {
            if i > 0 {
                rx.push('/');
            }
            if part.starts_with('{') && part.ends_with('}') {
                rx.push_str("([^\\/]*)");
            } else {
                rx.push_str(&regex::escape(part));
            }
        }
        rx.push('$');
        self.register_regex(Regex::new(&rx)?, callback)
    }
}

pub fn setup_navigator(files: &'static [EmuFile], navigator: &mut Navigator) -> Result<()> {
    navigator.add_db("Demozoo", files);

    navigator.register("All", |_path: &[&str], files: &'static [EmuFile]| {
        FilePickerSource::new(files)
    })?;
    navigator.register("Parties", |_path: &[&str], files: &'static [EmuFile]| {
        AllWordsSource::new(
            files
                .iter()
                .map(|f| f.get_party())
                .filter(|p| !p.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        )
    })?;
    navigator.register("Platforms", |_path: &[&str], files: &'static [EmuFile]| {
        AllWordsSource::new(
            files
                .iter()
                .map(|f| f.get_meta("platform"))
                .filter(|p| !p.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        )
    })?;
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
            let subset: Vec<u32> = files
                .iter()
                .enumerate()
                .filter(|(_, f)| f.get_party_and_compo() == (path[1], path[2]))
                .map(|(i, _)| i as u32)
                .collect();
            PickerSource::new(files, &subset)
        },
    )?;
    navigator.register("", |_path: &[&str], files: &'static [EmuFile]| {
        return AllWordsSource::new(["All".into(), "Parties".into(), "Platforms".into()].into());
    })?;
    Ok(())
}

pub fn setup_navigator_bevy(
    settings: Res<AppSettings>,
    mut navigator: ResMut<Navigator>,
) -> Result<()> {
    setup_navigator(settings.files, &mut navigator)?;
    navigator.go_root();
    Ok(())
}

pub(crate) fn handle_navigator(
    mut settings: ResMut<AppSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut writer: MessageWriter<CmdMessage>,
    mut navigator: ResMut<Navigator>,
    mut reader: MessageReader<FuzzyListSelect>,
    mut list_writer: MessageWriter<ShowFuzzyList>,
) {
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
    for msg in reader.read() {
        if msg.id == id {
            debug!("Selected {:?}", msg);
            if let Some(ef) = &msg.emu_file {
                settings.current_game = msg.item as isize;
                writer.write(CmdMessage(Cmd::Reload));
            } else {
                navigator.enter(&msg.text).show(&mut list_writer);
            }
            // settings.current_game = msg.item as isize;
            // Selected item in Navigator
            // Either push new Navigator or handle EmuFile
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
    use std::path::PathBuf;

    #[test]
    fn test_navigator() {
        let filter = DbFilter::default();
        let mut files = vec![];
        let path: PathBuf = "demos.txt".into();
        collect_db(&path, &filter, &mut files).unwrap();
        let files: &'static [EmuFile] = files.leak();
        let mut navigator = Navigator::new();
        setup_navigator(files, &mut navigator).unwrap();

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
}
