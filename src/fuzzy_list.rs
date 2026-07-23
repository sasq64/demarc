#![allow(clippy::type_complexity)]
//! A searchable list widget: a [`TextInput`] stacked on top of a [`TextList`].
//! As the user types, the list is filtered to the entries that match the query.
//!
//! Filtering is delegated to a [`FuzzySource`] so the matching strategy is
//! pluggable. The bundled [`SubstringSource`] does a simple case-insensitive
//! substring scan over an in-memory `Vec<String>` — fine for small/medium
//! lists. For large lists, implement [`FuzzySource`] over a prebuilt index, a
//! fuzzy matcher (e.g. `nucleo` / `fuzzy-matcher`), or an external database and
//! hand it to [`FuzzyList::spawn`]; nothing else changes.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::hud::TextList;
use crate::text_input::TextInput;

/// The number of results fetched from the source per query. A source may hold
/// far more items than can be shown; this caps how many we pull and render.
const DEFAULT_MAX_RESULTS: usize = 256;

/// Inner [`TextList`] ids are offset by this so they never collide with the
/// plain `TextList`s used elsewhere (which use small ids like 0/1). `FuzzyList`
/// consumes its inner list's [`TextListSelect`](crate::hud::TextListSelect) and
/// re-emits a [`FuzzyListSelect`], so this id stays private to the widget.
const INNER_LIST_ID_BASE: usize = 0x1000_0000;

/// One filtered result: the text to display plus a stable `id` identifying the
/// item in the underlying source (an index into a `Vec`, a database row id, …).
/// The `id` is what [`FuzzyListSelect`] reports, so callers get a handle that is
/// stable regardless of the current filter/order.
#[derive(Debug, Clone)]
pub struct FuzzyItem {
    pub id: usize,
    pub text: String,
}

/// Backs a [`FuzzyList`] with searchable items. Implement this to plug in
/// smarter matching without touching the widget: prefix trees, fuzzy scoring,
/// or an external index/database.
pub trait FuzzySource: Send + Sync + 'static {
    /// Return the items matching `query`, best match first, capped at `limit`.
    /// An empty/whitespace query should return the head of the full list (the
    /// unfiltered view).
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem>;
}

/// Simple in-memory source: case-insensitive substring match over a
/// `Vec<String>`. Lowercased copies are precomputed once so each keystroke is a
/// linear scan of cheap `contains` checks. Good enough up to a few thousand
/// entries; swap in an indexed source beyond that.
pub struct SubstringSource {
    items: Vec<String>,
    lowercased: Vec<String>,
}

impl SubstringSource {
    pub fn new(items: Vec<String>) -> Self {
        let lowercased = items.iter().map(|s| s.to_lowercase()).collect();
        Self { items, lowercased }
    }
}

impl From<Vec<String>> for SubstringSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl FuzzySource for SubstringSource {
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem> {
        let q = query.trim().to_lowercase();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| q.is_empty() || s.contains(&q))
            .take(limit)
            .map(|(i, _)| FuzzyItem {
                id: i,
                text: self.items[i].clone(),
            })
            .collect()
    }
}

/// In-memory source that matches on all query words independently: the query is
/// split on whitespace and an item matches only if it contains every word as a
/// (case-insensitive) substring, in any order. So `"na an"` matches `"banana"`.
/// Like [`SubstringSource`] it precomputes lowercased copies and scans linearly;
/// good up to a few thousand entries.
pub struct AllWordsSource {
    items: Vec<String>,
    lowercased: Vec<String>,
}

impl AllWordsSource {
    pub fn new(items: Vec<String>) -> Self {
        let lowercased = items.iter().map(|s| s.to_lowercase()).collect();
        Self { items, lowercased }
    }
}

impl From<Vec<String>> for AllWordsSource {
    fn from(items: Vec<String>) -> Self {
        Self::new(items)
    }
}

impl FuzzySource for AllWordsSource {
    fn search(&self, query: &str, limit: usize) -> Vec<FuzzyItem> {
        let q = query.to_lowercase();
        let words: Vec<&str> = q.split_whitespace().collect();
        self.lowercased
            .iter()
            .enumerate()
            .filter(|(_, s)| words.iter().all(|w| s.contains(w)))
            .take(limit)
            .map(|(i, _)| FuzzyItem {
                id: i,
                text: self.items[i].clone(),
            })
            .collect()
    }
}

/// Emitted when the user picks a row (Enter) in a [`FuzzyList`].
#[derive(Message, Debug, Clone)]
pub struct FuzzyListSelect {
    /// The [`FuzzyList`]'s `id`, so callers can tell widgets apart.
    pub id: usize,
    /// Stable id of the chosen item (see [`FuzzyItem::id`]).
    pub item: usize,
    /// The chosen item's display text, for convenience.
    pub text: String,
}

/// Remembers the last query typed into each [`FuzzyList`], keyed by its `id`,
/// so a widget that is closed and re-opened comes back with the same search
/// text (and therefore the same filtered view). [`FuzzyList::sync_filter`] keeps
/// this up to date; pass the stored value to [`FuzzyList::spawn`] as the initial
/// query to restore it.
#[derive(Resource, Default)]
pub struct FuzzyQueryStore(HashMap<usize, String>);

impl FuzzyQueryStore {
    /// The last query seen for `id`, or `""` if none has been recorded yet.
    pub fn get(&self, id: usize) -> &str {
        self.0.get(&id).map(String::as_str).unwrap_or("")
    }
}

/// A searchable list: a [`TextInput`] search box above a filtered [`TextList`].
///
/// Spawn with [`FuzzyList::spawn`]; despawn the returned (root) entity to close
/// it. Type to filter; Up/Down/PageUp/PageDown/Home/End navigate; Enter emits a
/// [`FuzzyListSelect`].
#[derive(Component)]
pub struct FuzzyList {
    /// Caller-chosen id, echoed back in [`FuzzyListSelect`].
    pub id: usize,
    source: Box<dyn FuzzySource>,
    /// The results currently shown, in display order. Maps the inner list's
    /// visible index → source item when reporting a selection.
    shown: Vec<FuzzyItem>,
    /// Last query we filtered on. We poll the input's text against this rather
    /// than using change detection, because [`TextInput`] bypasses change
    /// detection while the user types.
    last_query: String,
    /// Child [`TextInput`] entity (the search box).
    input: Entity,
    /// Child [`TextList`] entity (the results).
    list: Entity,
    /// The inner list's [`TextList::id`], used to route its selections.
    list_id: usize,
    max_results: usize,
}

impl FuzzyList {
    /// Spawns a `FuzzyList` and returns its root entity. Despawn that entity to
    /// close the widget (its children go with it).
    ///
    /// `source` is anything implementing [`FuzzySource`]; pass a
    /// `SubstringSource::from(items)` for the simple built-in behaviour.
    /// `initial_query` pre-fills the search box and filters the initial view —
    /// pass `""` for a fresh unfiltered list, or a value from
    /// [`FuzzyQueryStore`] to restore a previously closed widget's search.
    pub fn spawn(
        id: usize,
        commands: &mut Commands,
        font: Handle<Font>,
        source: impl FuzzySource,
        visible_count: usize,
        width: f32,
        initial_query: &str,
    ) -> Entity {
        let source: Box<dyn FuzzySource> = Box::new(source);
        let list_id = INNER_LIST_ID_BASE + id;

        // Full-screen container that centers the widget.
        let overlay = commands
            .spawn(Node {
                position_type: PositionType::Absolute,
                top: Val::Px(0.0),
                left: Val::Px(0.0),
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            })
            .id();

        // Vertical stack: search box on top, results below.
        let stack = commands
            .spawn(Node {
                flex_direction: FlexDirection::Column,
                width: Val::Px(width),
                row_gap: Val::Px(6.0),
                ..default()
            })
            .id();
        commands.entity(overlay).add_child(stack);

        // Search box. `ignore_enter` leaves Enter for the list to consume as a
        // selection instead of submitting/clearing the input.
        let input = commands
            .spawn((
                Node {
                    padding: UiRect::all(Val::Px(16.0)),
                    border: UiRect::all(Val::Px(2.0)),
                    ..default()
                },
                BackgroundColor(Color::linear_rgba(0.0, 0.0, 0.0, 0.9)),
                BorderColor::all(Color::linear_rgba(1.0, 0.4, 0.2, 0.9)),
                TextInput {
                    text: initial_query.to_string(),
                    showing: true,
                    ignore_enter: true,
                },
            ))
            .id();
        commands.entity(stack).add_child(input);

        // Initial results (filtered by `initial_query`, if any), and the list
        // box to show them.
        let shown = source.search(initial_query, DEFAULT_MAX_RESULTS);
        let items = shown.iter().map(|r| r.text.clone()).collect();
        let list = TextList::spawn_box(commands, stack, list_id, font, items, visible_count, width);

        commands.entity(stack).insert(FuzzyList {
            id,
            source,
            shown,
            last_query: initial_query.to_string(),
            input,
            list,
            list_id,
            max_results: DEFAULT_MAX_RESULTS,
        });

        overlay
    }

    /// Re-filters each widget whose search text changed since last frame.
    fn sync_filter(
        mut lists: Query<&mut FuzzyList>,
        inputs: Query<&TextInput>,
        mut text_lists: Query<&mut TextList>,
        mut store: ResMut<FuzzyQueryStore>,
    ) {
        for mut fuzzy in &mut lists {
            let Ok(input) = inputs.get(fuzzy.input) else {
                continue;
            };
            if input.text == fuzzy.last_query {
                continue;
            }
            let query = input.text.clone();
            let results = fuzzy.source.search(&query, fuzzy.max_results);

            if let Ok(mut list) = text_lists.get_mut(fuzzy.list) {
                list.items = results.iter().map(|r| r.text.clone()).collect();
                list.selected = 0;
                list.scroll_position = 0;
            }
            // Remember the query so re-opening this widget restores it.
            store.0.insert(fuzzy.id, query.clone());
            fuzzy.shown = results;
            fuzzy.last_query = query;
        }
    }

    /// Translates an inner [`TextList`] selection into a [`FuzzyListSelect`],
    /// mapping the visible row back to its stable source item.
    fn relay_select(
        mut reader: MessageReader<crate::hud::TextListSelect>,
        lists: Query<&FuzzyList>,
        mut writer: MessageWriter<FuzzyListSelect>,
    ) {
        for msg in reader.read() {
            for fuzzy in &lists {
                if fuzzy.list_id != msg.id {
                    continue;
                }
                if let Some(item) = fuzzy.shown.get(msg.index) {
                    writer.write(FuzzyListSelect {
                        id: fuzzy.id,
                        item: item.id,
                        text: item.text.clone(),
                    });
                }
            }
        }
    }
}

pub struct FuzzyListPlugin;

impl Plugin for FuzzyListPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<FuzzyListSelect>()
            .init_resource::<FuzzyQueryStore>()
            .add_systems(
                Update,
                (
                    FuzzyList::sync_filter,
                    // Runs after the inner TextList has produced its select message.
                    FuzzyList::relay_select.after(TextList::update_keys),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud::{HudPlugin, TextList};
    use crate::text_input::TextInputPlugin;

    #[derive(Resource, Default)]
    struct Selected(Vec<FuzzyListSelect>);

    fn collect(mut reader: MessageReader<FuzzyListSelect>, mut out: ResMut<Selected>) {
        for msg in reader.read() {
            out.0.push(msg.clone());
        }
    }

    fn setup() -> App {
        let mut app = App::new();
        app.add_plugins((HudPlugin, TextInputPlugin, FuzzyListPlugin))
            .add_message::<bevy::input::keyboard::KeyboardInput>()
            .add_message::<bevy::window::WindowResized>()
            .init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<Time>()
            .init_resource::<Selected>()
            .add_systems(Update, collect.after(FuzzyList::relay_select));
        app
    }

    fn items() -> Vec<String> {
        ["apple", "apricot", "banana", "cherry", "grape"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    fn spawn(app: &mut App) -> Entity {
        let font = Handle::<Font>::default();
        let root = FuzzyList::spawn(
            7,
            &mut app.world_mut().commands(),
            font,
            SubstringSource::new(items()),
            5,
            400.0,
            "",
        );
        app.update();
        root
    }

    /// The `FuzzyList` component lives on the inner stack entity, not the root.
    fn fuzzy_entity(app: &mut App) -> Entity {
        let mut q = app.world_mut().query_filtered::<Entity, With<FuzzyList>>();
        q.iter(app.world()).next().expect("no FuzzyList spawned")
    }

    fn input_entity(app: &mut App) -> Entity {
        let e = fuzzy_entity(app);
        app.world().get::<FuzzyList>(e).unwrap().input
    }

    fn list_items(app: &mut App) -> Vec<String> {
        let e = fuzzy_entity(app);
        let list = app.world().get::<FuzzyList>(e).unwrap().list;
        app.world().get::<TextList>(list).unwrap().items.clone()
    }

    /// Set the search text the way a keystroke would (bypassing change
    /// detection, as `TextInput::on_input` does) and let `sync_filter` react.
    fn type_query(app: &mut App, text: &str) {
        let input = input_entity(app);
        app.world_mut().get_mut::<TextInput>(input).unwrap().text = text.to_string();
        app.update();
    }

    #[test]
    fn starts_unfiltered() {
        let mut app = setup();
        spawn(&mut app);
        assert_eq!(list_items(&mut app), items());
    }

    #[test]
    fn filters_to_matching_entries() {
        let mut app = setup();
        spawn(&mut app);

        type_query(&mut app, "ap");
        assert_eq!(list_items(&mut app), vec!["apple", "apricot", "grape"]);

        type_query(&mut app, "err");
        assert_eq!(list_items(&mut app), vec!["cherry"]);
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let mut app = setup();
        spawn(&mut app);
        type_query(&mut app, "BAN");
        assert_eq!(list_items(&mut app), vec!["banana"]);
    }

    #[test]
    fn clearing_query_restores_full_list() {
        let mut app = setup();
        spawn(&mut app);
        type_query(&mut app, "ap");
        type_query(&mut app, "");
        assert_eq!(list_items(&mut app), items());
    }

    #[test]
    fn query_survives_close_and_reopen() {
        let mut app = setup();
        let root = spawn(&mut app);

        // Type a filter, then close the widget.
        type_query(&mut app, "ap");
        assert_eq!(list_items(&mut app), vec!["apple", "apricot", "grape"]);
        app.world_mut().entity_mut(root).despawn();
        app.update();

        // Re-open, restoring the remembered query the way the real call site
        // does, and confirm both the search text and the filtered view return.
        let restored = app.world().resource::<FuzzyQueryStore>().get(7).to_string();
        assert_eq!(restored, "ap");
        FuzzyList::spawn(
            7,
            &mut app.world_mut().commands(),
            Handle::<Font>::default(),
            SubstringSource::new(items()),
            5,
            400.0,
            &restored,
        );
        app.update();

        let input = input_entity(&mut app);
        assert_eq!(app.world().get::<TextInput>(input).unwrap().text, "ap");
        assert_eq!(list_items(&mut app), vec!["apple", "apricot", "grape"]);
    }

    #[test]
    fn all_words_source_matches_every_word_in_any_order() {
        let src = AllWordsSource::new(items());

        // Empty query returns everything.
        let all: Vec<String> = src.search("", 256).into_iter().map(|r| r.text).collect();
        assert_eq!(all, items());

        // Two words, out of order, both as substrings of the same item.
        let hits: Vec<String> = src
            .search("na an", 256)
            .into_iter()
            .map(|r| r.text)
            .collect();
        assert_eq!(hits, vec!["banana"]);

        // A word matching nothing filters the item out even if others match.
        assert!(src.search("apple zzz", 256).is_empty());
    }

    #[test]
    fn selection_reports_stable_source_id_after_filtering() {
        let mut app = setup();
        spawn(&mut app);

        // Filter so "cherry" (source id 3) is the only, and selected, row.
        type_query(&mut app, "cherry");
        assert_eq!(list_items(&mut app), vec!["cherry"]);

        // Emit the inner list's selection of visible row 0 and let relay run.
        let list_id = INNER_LIST_ID_BASE + 7;
        app.world_mut().write_message(crate::hud::TextListSelect {
            id: list_id,
            index: 0,
        });
        app.update();

        let selected = &app.world().resource::<Selected>().0;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, 7);
        assert_eq!(selected[0].item, 3, "should map back to source index");
        assert_eq!(selected[0].text, "cherry");
    }
}
