use bevy::prelude::*;

use crate::AppState;
use crate::config::AppSettings;
use crate::egui_ui::HudState;
use crate::loading::LoadFile;
use crate::playlists::{FAVORITES, Playlists};
use crate::ui::{FuzzyListSelect, ShowFuzzyList};
use ::navigator::{PLAYLIST_MENU, PLAYLIST_ROOT_MENU, ShowList, setup_navigator};
use retro_ui::TitleClick;

/// The shared navigator, as a resource.
#[derive(Resource, Deref, DerefMut)]
pub(crate) struct Navigator(::navigator::Navigator);

fn write_list(list: ShowList, lw: &mut MessageWriter<ShowFuzzyList>) {
    lw.write(ShowFuzzyList {
        id: list.id,
        source: list.source,
        prompt: Some(list.prompt),
        selected: list.selected,
        title: list.title,
        title_select: list.title_select,
        keep_open: true,
    });
}

impl Navigator {
    pub(crate) fn new() -> Self {
        Self(::navigator::Navigator::new())
    }

    pub(crate) fn show(&mut self, lw: &mut MessageWriter<ShowFuzzyList>) {
        if let Some(list) = self.take_show() {
            write_list(list, lw);
        }
    }

    pub(crate) fn open(&mut self, lw: &mut MessageWriter<ShowFuzzyList>) {
        if let Some(list) = self.current() {
            write_list(list, lw);
        }
    }

    fn remember_hud(&mut self, hud: &HudState) {
        let id = self.stack[self.pos as usize].id;
        if let Some(query) = hud.list_query(id) {
            let selected = hud.list_selected_item(id);
            self.remember_state(query, selected);
        }
    }
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
    navigator.remember_hud(&hud);
    let id = navigator.stack[navigator.pos as usize].id;
    if hud.list_query(id).is_none() {
        return;
    }
    if let Some(click) = hud.take_title_click() {
        match click {
            TitleClick::Depth(depth) => navigator.to_depth(depth),
            TitleClick::Back => navigator.back(),
            TitleClick::Forward => navigator.forward(),
        };
        navigator.show(&mut list_writer);
    } else if input.just_pressed(KeyCode::ArrowLeft) {
        navigator.back();
        navigator.show(&mut list_writer);
    } else if input.just_pressed(KeyCode::ArrowRight) {
        navigator.forward();
        navigator.show(&mut list_writer);
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
    let path = current.path.clone();
    for msg in reader.read() {
        if msg.id == id && id == PLAYLIST_ROOT_MENU {
            let name = path.join("/");
            if let Some(index) = playlists.find(&name)
                && playlists.delete(index)
            {
                navigator.remove_playlist(&name);
            }
            navigator.pop();
            navigator.show(&mut list_writer);
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
                navigator.back();
                navigator.show(&mut list_writer);
            } else {
                navigator.enter(&["dls"]);
                navigator.show(&mut list_writer);
            }
        } else if msg.id == id {
            debug!("Selected {:?}", msg);
            if let Some(emu_file) = msg.emu_file.clone() {
                if msg.alt && path.len() == 1 && navigator.playlists.contains(&path[0]) {
                    navigator.open_playlist_menu(msg.item);
                    navigator.show(&mut list_writer);
                    continue;
                }
                if msg.alt {
                    let id = msg.item;
                    navigator.enter(&[&id.to_string(), "dls"]);
                    navigator.show(&mut list_writer);
                    continue;
                }
                if let Some(file_index) = navigator.launch(msg.item) {
                    settings.current_game = file_index as isize;
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
                navigator.open_playlist_root_menu(&msg.text);
                navigator.show(&mut list_writer);
            } else {
                navigator.enter(&[&msg.text]);
                navigator.show(&mut list_writer);
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
