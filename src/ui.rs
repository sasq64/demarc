use std::{sync::Arc, time::Duration};

use bevy::prelude::*;

use crate::emu_file::EmuFile;
use crate::fuzzy_list::FuzzySource;

/// What the pickers in this app are lists *of*. Every source handed to
/// [`ShowFuzzyList`] agrees on this one type, so a caller holding the `item` of
/// a [`FuzzyListSelect`] can ask the source for the entry behind it
/// ([`FuzzySource::get_data`]) instead of keeping its own copy of the list.
/// A source whose rows are not entries at all -- the hotkey list, say -- has no
/// data to hand back and simply inherits the default.
pub type ListSource = Arc<dyn FuzzySource<EmuFile>>;

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
    Error,
}

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
