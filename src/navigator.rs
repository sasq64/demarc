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

    fn goto(&mut self, path: &str) {
        for (key, val) in &self.mapping {
            if let Some(m) = key.captures(path) {
                let groups: Vec<&str> = m.iter().map(|m| m.map_or("", |m| m.as_str())).collect();
                let source = val(&groups, self.files["Demozoo"]);
                self.push(NavList { id: 0, source });
                return;
            }
        }
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
    pub fn register<S: FuzzySource<EmuFile>>(
        &mut self,
        pattern: &str,
        callback: impl Fn(&[&str], &'static [EmuFile]) -> S + Send + Sync + 'static,
    ) -> Result<()> {
        self.mapping.push((
            Regex::new(pattern)?,
            Box::new(move |groups, files| Arc::new(callback(groups, files))),
        ));
        Ok(())
    }
}

fn setup_navigator(settings: Res<AppSettings>, mut navigator: ResMut<Navigator>) {
    navigator.add_db("Demozoo", settings.files);

    navigator.register("Parties", |_path: &[&str], files: &'static [EmuFile]| {
        AllWordsSource::new(
            files
                .iter()
                .filter_map(|f| f.meta.get("party").copied())
                .filter(|p| !p.is_empty())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(String::from)
                .collect(),
        )
    });
    navigator.register("All", |_path: &[&str], files: &'static [EmuFile]| {
        FilePickerSource::new(files)
    });

    navigator.register(
        "Parties/{name}",
        |path: &[&str], files: &'static [EmuFile]| {
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
    );
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
    );
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
            debug!("Selected {}", msg.text);
            if msg.id == 98 {
                // Selected party
                let files = settings.files;
                let subset: Vec<u32> = files
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| f.meta.get("party").copied().unwrap_or("") == msg.text)
                    .map(|(i, _)| i as u32)
                    .collect();
                navigator
                    .push(NavList {
                        id: 99,
                        source: Arc::new(PickerSource::new(files, &subset)),
                    })
                    .show(&mut list_writer);
            } else if msg.text == "Demozoo" {
                navigator
                    .push(NavList {
                        id: 99,
                        source: Arc::new(FilePickerSource::new(settings.files)),
                    })
                    .show(&mut list_writer);
            } else if msg.text == "Parties" {
                let parties: Vec<String> = settings
                    .files
                    .iter()
                    .filter_map(|f| f.meta.get("party").copied())
                    .filter(|p| !p.is_empty())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .map(String::from)
                    .collect();
                navigator
                    .push(NavList {
                        id: 98,
                        source: Arc::new(AllWordsSource::new(parties)),
                    })
                    .show(&mut list_writer);
            } else {
                if msg.id == 99 {
                    settings.current_game = msg.item as isize;
                } else {
                    settings.current_game = msg.item as isize;
                }
                writer.write(CmdMessage(Cmd::Reload));
                // Selected item in Navigator
                // Either push new Navigator or handle EmuFile
            }
        }
    }
}

#[cfg(test)]
mod test {
    use crate::files::{DbFilter, collect_db};
    use std::path::PathBuf;

    #[test]
    fn test_navigator() {
        let filter = DbFilter::default();
        let mut files = vec![];
        let path: PathBuf = "../demodb/demozoo.txt".into();
        collect_db(&path, &filter, &mut files).unwrap();
    }
}
