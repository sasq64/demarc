//! RightAlt/RightCtrl+V menu that writes fixes for the playing release into the repo's
//! `system/overrides.toml`, for working on overrides from a checkout.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bevy::prelude::*;
use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, value};

use crate::config::AppSettings;
use crate::emu_file::{DemoId, url_file_name};
use crate::emulator::{Emulator, Recorded};
use crate::frontend::EmuView;
use crate::fuzzy_list::AllWordsSource;
use crate::overrides;
use crate::ui::{FuzzyListSelect, HudLocation, SetHudText, ShowFuzzyList, UiState};

const MENU_ID: usize = 97;

const ITEMS: &[&str] = &[
    "Apply wine_res",
    "Apply dos4gw",
    "Apply boot file",
    "Apply download",
    "Add Enter x 3",
    "Record Events",
];
const RECORD: usize = 5;

pub struct PatchMenuPlugin;

impl Plugin for PatchMenuPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (open_menu, handle_select));
    }
}

fn overrides_path() -> PathBuf {
    Path::new("system").join(overrides::FILE_NAME)
}

fn open_menu(
    input: Res<ButtonInput<KeyCode>>,
    ui_state: Res<UiState>,
    emus: Query<(&Emulator, &EmuView)>,
    mut settings: ResMut<AppSettings>,
    mut show_list: MessageWriter<ShowFuzzyList>,
) {
    let hot_key = input.any_pressed([KeyCode::AltRight, KeyCode::ControlRight]);
    if !hot_key
        || !input.just_pressed(KeyCode::KeyV)
        || ui_state.modal
        || !overrides_path().is_file()
    {
        return;
    }
    // Keeps the hot key's release from also opening the command list.
    settings.hotkey_pressed_at = 0.0;
    let recording = current(&emus, &settings).is_some_and(|emu| emu.recording.is_some());
    let mut items: Vec<String> = ITEMS.iter().map(|s| s.to_string()).collect();
    if recording {
        items[RECORD] = "Commit Events".into();
    }
    show_list.write(ShowFuzzyList {
        id: MENU_ID,
        source: Arc::new(AllWordsSource::new(items)),
        prompt: None,
        selected: None,
        title: "Patch overrides".into(),
        title_select: -1,
    });
}

fn handle_select(
    mut reader: MessageReader<FuzzyListSelect>,
    mut emus: Query<(&mut Emulator, &EmuView)>,
    mut settings: ResMut<AppSettings>,
    mut hud: MessageWriter<SetHudText>,
) {
    for msg in reader.read() {
        if msg.id != MENU_ID {
            continue;
        }
        let Some((mut emu, _)) = emus
            .iter_mut()
            .find(|(emu, view)| !emu.is_crossfade && view.index == settings.current_emu)
        else {
            continue;
        };
        let text = match patch(&mut emu, msg.item, &mut settings) {
            Ok(text) => text,
            Err(err) => {
                warn!("Patch menu: {err:#}");
                format!("{err:#}")
            }
        };
        hud.write(SetHudText {
            text,
            duration: Duration::from_secs(2),
            location: HudLocation::TopLeft,
            ..Default::default()
        });
    }
}

fn current<'a>(
    emus: &'a Query<(&Emulator, &EmuView)>,
    settings: &AppSettings,
) -> Option<&'a Emulator> {
    emus.iter()
        .find(|(emu, view)| !emu.is_crossfade && view.index == settings.current_emu)
        .map(|(emu, _)| emu)
}

#[derive(Debug, PartialEq)]
enum Change {
    Meta(&'static str, String),
    Boot(String),
    File(String),
    Enters,
    Events(Vec<(u64, Recorded)>),
}

fn change_for(emu: &Emulator, item: usize) -> Result<Change> {
    Ok(match item {
        0 => {
            let res = emu.work_file.get_meta_or("wine_res", "");
            if res.is_empty() {
                bail!("No wine_res");
            }
            Change::Meta("wine_res", res)
        }
        1 => Change::Meta("dos4gw", "true".into()),
        2 => {
            let path = &emu.work_file.path;
            let name = path
                .file_name()
                .filter(|_| path.is_file())
                .context("No boot file")?;
            Change::Boot(name.to_string_lossy().into_owned())
        }
        3 => {
            let fetched = emu.work_file.get_meta_or("fetched", "");
            let url = fetched.split(';').next().unwrap_or_default();
            let name = url_file_name(url).context("No download")?;
            Change::File(name)
        }
        4 => Change::Enters,
        _ => bail!("Unknown patch"),
    })
}

fn patch(emu: &mut Emulator, item: usize, settings: &mut AppSettings) -> Result<String> {
    let id = emu.emu_file.demo_id().context("No db id")?;
    let change = if item == RECORD {
        match emu.recording.take() {
            Some((_, events)) => Change::Events(events),
            None => {
                let frame = emu.core.as_ref().context("No core")?.frames_stepped();
                emu.recording = Some((frame, Vec::new()));
                return Ok("Recording events".into());
            }
        }
    } else {
        change_for(emu, item)?
    };
    let path = overrides_path();
    let text = std::fs::read_to_string(&path)?;
    let mut doc: DocumentMut = text.parse().context("Can't parse overrides")?;
    edit(&mut doc, id, &change)?;
    let text = doc.to_string();
    let over = overrides::parse(&text, Emulator::resolve_key_name)?
        .remove(&id)
        .context("Entry did not parse")?;
    std::fs::write(&path, text)?;
    settings.overrides.insert(id, over);
    let name = if item == RECORD { "Commit Events" } else { ITEMS[item] };
    Ok(format!("{name} -> [{}.{}]", id.db, id.id))
}

fn edit(doc: &mut DocumentMut, id: DemoId, change: &Change) -> Result<()> {
    let db = doc
        .entry(id.db)
        .or_insert_with(|| {
            let mut table = Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_mut()
        .context("db is not a table")?;
    let entry = db
        .entry(&id.id.to_string())
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .context("entry is not a table")?;
    match change {
        Change::Meta(key, val) => {
            let meta = entry
                .entry("meta")
                .or_insert(value(InlineTable::new()))
                .as_table_like_mut()
                .context("meta is not a table")?;
            meta.insert(key, value(val.as_str()));
            meta.fmt();
        }
        Change::Boot(name) => {
            entry.insert("boot", value(name.as_str()));
        }
        Change::File(name) => {
            entry.insert("file", value(name.as_str()));
        }
        Change::Enters => {
            let events = entry
                .entry("events")
                .or_insert(value(Array::new()))
                .as_array_mut()
                .context("events is not an array")?;
            for frame in [50i64, 60, 70] {
                let mut event = InlineTable::new();
                event.insert("frame", frame.into());
                event.insert("key", "Enter".into());
                events.push(event);
            }
        }
        Change::Events(recorded) => {
            let mut events = Array::new();
            for (frame, rec) in recorded {
                let mut event = InlineTable::new();
                event.insert("frame", (*frame as i64).into());
                match rec {
                    Recorded::Key(code) => {
                        event.insert("key", key_name(*code).into());
                    }
                    Recorded::Click(p) => {
                        let round = |v: f32| ((v * 1000.0).round() / 1000.0) as f64;
                        let mut pos = Array::new();
                        pos.push(round(p.x));
                        pos.push(round(p.y));
                        event.insert("click", pos.into());
                    }
                }
                events.push(event);
            }
            entry.insert("events", value(events));
        }
    }
    Ok(())
}

/// `KeyA` -> `A`, `Digit5` -> `5`, everything else as its `KeyCode` name.
fn key_name(code: KeyCode) -> String {
    let name = format!("{code:?}");
    ["Key", "Digit"]
        .iter()
        .find_map(|p| name.strip_prefix(p).filter(|rest| rest.len() == 1))
        .map_or(name.clone(), str::to_string)
}

#[cfg(test)]
#[path = "tests/patch_menu_tests.rs"]
mod tests;
