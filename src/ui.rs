use std::{sync::Arc, time::Duration};

use bevy::input::ButtonState;
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::prelude::*;

use crate::emu_file::EmuFile;
use crate::fuzzy_list::FuzzySource;

pub use retro_ui::HudLocation;

/// What the pickers in this app are lists *of*. Every source handed to
/// [`ShowFuzzyList`] agrees on this one type, so a caller holding the `item` of
/// a [`FuzzyListSelect`] can ask the source for the entry behind it
/// ([`FuzzySource::get_data`]) instead of keeping its own copy of the list.
/// A source whose rows are not entries at all -- the hotkey list, say -- has no
/// data to hand back and simply inherits the default.
pub type ListSource = Arc<dyn FuzzySource<EmuFile>>;

/// Opens the searchable list over `source`. Picking a row emits a
/// [`FuzzyListSelect`] carrying `id` back, so several callers can tell their
/// pickers apart; re-opening the same `id` restores the search text and the
/// selected row.
#[derive(Message, Clone)]
pub struct ShowFuzzyList {
    pub id: usize,
    pub source: ListSource,
    pub prompt: Option<String>,
    /// Source id of the row to highlight, if it is among the results.
    pub selected: Option<usize>,
    /// Shown above the search box, and hidden while empty.
    pub title: String,
    // Highlight this part of the title
    pub title_select: i32,
}

/// Emitted when the user picks a row (Enter, or Shift+Enter — see
/// [`FuzzyListSelect::alt`]) in the list opened by [`ShowFuzzyList`].
#[derive(Message, Debug, Clone)]
pub struct FuzzyListSelect {
    /// The list's `id`, so callers can tell their pickers apart.
    pub id: usize,
    /// Stable id of the chosen item, as reported by [`FuzzySource::search`].
    pub item: usize,
    #[allow(dead_code)]
    pub text: String,
    /// Set when the row was picked with Shift held (Shift+Enter), asking the
    /// caller for its alternative action on the item rather than the default.
    pub alt: bool,
    pub emu_file: Option<EmuFile>,
}

#[derive(Default, Message, Clone)]
pub struct SetHudText {
    pub text: String,
    pub delay: Duration,
    pub duration: Duration,
    pub location: HudLocation,
}

/// Whether *any* modal UI owns the keyboard -- the picker or a settings
/// dialog -- so keys should not go to the emulated machine.
#[derive(Resource, Default, PartialEq)]
pub struct UiState {
    pub modal: bool,
}

/// Best-effort logical key for an injected press. The egui dialogs read this
/// for text entry; the emulator only looks at `key_code`.
fn logical_key(key: KeyCode) -> Key {
    let name = format!("{key:?}");
    if let Some(letter) = name.strip_prefix("Key")
        && letter.len() == 1
    {
        return Key::Character(letter.to_lowercase().into());
    }
    if let Some(digit) = name.strip_prefix("Digit")
        && digit.len() == 1
    {
        return Key::Character(digit.into());
    }
    match key {
        KeyCode::Enter | KeyCode::NumpadEnter => Key::Enter,
        KeyCode::Space => Key::Space,
        KeyCode::Tab => Key::Tab,
        KeyCode::Escape => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
        KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
        KeyCode::SuperLeft | KeyCode::SuperRight => Key::Super,
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

pub(crate) fn key_message(key: KeyCode, state: ButtonState, window: Entity) -> KeyboardInput {
    KeyboardInput {
        key_code: key,
        logical_key: logical_key(key),
        state,
        text: None,
        repeat: false,
        window,
    }
}
