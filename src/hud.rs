#![allow(clippy::type_complexity)]

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;

#[derive(Message)]
pub struct TextListSelect {
    pub id: usize,
    pub index: usize,
}

/// A scrollable list of strings rendered inside a semi-transparent bordered box.
#[derive(Default, Component)]
pub struct TextList {
    pub id: usize,
    pub items: Vec<String>,
    pub scroll_position: usize,
    pub visible_count: usize,
    /// Index into `items` of the currently selected row.
    pub selected: usize,
    pub controlled: bool,
}

const SELECTED_ROW_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.25);
const ROW_FONT_SIZE: f32 = 20.0;
/// Fixed height of every row, slightly above the natural line height for
/// [`ROW_FONT_SIZE`]. Rows keep this height even when empty, so the box does not
/// resize as the list is filtered or emptied.
const ROW_HEIGHT: f32 = ROW_FONT_SIZE * 1.1;

/// Marks a child text entity of a [`TextList`] and records which visible row it is.
#[derive(Component)]
struct TextListRow(usize);

impl TextList {
    /// Spawns a `TextList` and its row text entities, returning the container entity.
    ///
    /// The caller can insert/override the [`Node`] on the returned entity to position it.
    pub fn spawn(
        id: usize,
        commands: &mut Commands,
        font: Handle<Font>,
        items: Vec<String>,
        visible_count: usize,
        width: f32,
    ) -> Entity {
        // Full-screen container that centers the content-sized box; the returned
        // entity is the box itself (the one carrying `TextList`).
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
        Self::spawn_box(commands, overlay, id, font, items, visible_count, width)
    }

    /// Spawns just the bordered list box (and its row text entities) as a child
    /// of `parent`, returning the box entity carrying [`TextList`]. Shared by
    /// [`TextList::spawn`] and other widgets (e.g. `FuzzyList`) that embed a
    /// list inside their own layout.
    pub fn spawn_box(
        commands: &mut Commands,
        parent: Entity,
        id: usize,
        font: Handle<Font>,
        items: Vec<String>,
        visible_count: usize,
        width: f32,
    ) -> Entity {
        let mut box_entity = Entity::PLACEHOLDER;
        commands.entity(parent).with_children(|parent| {
            box_entity = parent
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        width: Val::Px(width),
                        padding: UiRect::all(Val::Px(16.0)),
                        border: UiRect::all(Val::Px(2.0)),
                        row_gap: Val::Px(4.0),
                        // Rows lay their text out on a single line (`NoWrap`), so
                        // anything too long spills sideways; clip it at the content
                        // box (inside the padding) instead of letting it escape.
                        overflow: Overflow::clip_x(),
                        overflow_clip_margin: OverflowClipMargin::content_box(),
                        ..default()
                    },
                    BackgroundColor(Color::linear_rgba(0.0, 0.0, 0.0, 0.9)),
                    BorderColor::all(Color::linear_rgba(1.0, 0.4, 0.2, 0.9)),
                    TextList {
                        id,
                        items,
                        visible_count,
                        controlled: true,
                        ..Default::default()
                    },
                ))
                .with_children(|box_node| {
                    for i in 0..visible_count {
                        box_node.spawn((
                            Node {
                                width: Val::Percent(100.0),
                                // Without this the row would be sized to fit its
                                // unwrapped (min-content) text and push past the box.
                                min_width: Val::Px(0.0),
                                height: Val::Px(ROW_HEIGHT),
                                flex_shrink: 0.0,
                                ..default()
                            },
                            Text::new(""),
                            TextFont {
                                font: font.clone().into(),
                                font_size: FontSize::Px(ROW_FONT_SIZE),
                                ..default()
                            },
                            TextColor(Color::WHITE),
                            TextLayout {
                                justify: Justify::Left,
                                linebreak: LineBreak::NoWrap,
                            },
                            BackgroundColor(Color::NONE),
                            TextListRow(i),
                        ));
                    }
                })
                .id();
        });
        box_entity
    }
    pub(crate) fn update_keys(
        mut messages: MessageReader<KeyboardInput>,
        mut lists: Query<&mut TextList>,
        mut writer: MessageWriter<TextListSelect>,
    ) {
        for mut list in &mut lists {
            if list.controlled {
                if list.items.is_empty() {
                    continue;
                }
                for msg in messages.read() {
                    if msg.state == ButtonState::Released {
                        continue;
                    }
                    let last = list.items.len() - 1;
                    let page = list.visible_count.max(1);
                    match msg.key_code {
                        KeyCode::ArrowUp => list.selected = list.selected.saturating_sub(1),
                        KeyCode::ArrowDown => list.selected = (list.selected + 1).min(last),
                        KeyCode::PageUp => list.selected = list.selected.saturating_sub(page),
                        KeyCode::PageDown => list.selected = (list.selected + page).min(last),
                        KeyCode::Home => list.selected = 0,
                        KeyCode::End => list.selected = last,
                        KeyCode::Enter => {
                            writer.write(TextListSelect {
                                id: list.id,
                                index: list.selected,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    fn update_text_list(
        mut lists: Query<(&mut TextList, &Children), Changed<TextList>>,
        mut rows: Query<(&TextListRow, &mut Text, &mut BackgroundColor)>,
    ) {
        for (mut list, children) in &mut lists {
            // Scroll so the selected item is within the visible window.
            if list.visible_count > 0 {
                if list.selected < list.scroll_position {
                    list.scroll_position = list.selected;
                } else if list.selected >= list.scroll_position + list.visible_count {
                    list.scroll_position = list.selected + 1 - list.visible_count;
                }
            }
            for child in children.iter() {
                if let Ok((row, mut text, mut bg)) = rows.get_mut(child) {
                    let idx = list.scroll_position + row.0;
                    text.0 = list.items.get(idx).cloned().unwrap_or_default();
                    bg.0 = if idx == list.selected && idx < list.items.len() {
                        SELECTED_ROW_COLOR
                    } else {
                        Color::NONE
                    };
                }
            }
        }
    }
}

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<TextListSelect>().add_systems(
            Update,
            (
                TextList::update_text_list,
                TextList::update_keys.run_if(on_message::<KeyboardInput>),
            ),
        );
    }
}
