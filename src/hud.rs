use std::collections::HashMap;
use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResized};
use bevy_tweening::lens::TextColorLens;
use bevy_tweening::{CycleCompletedEvent, Delay, Tween, TweenAnim};

#[derive(Component)]
pub struct InfoText;

#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
}

#[derive(Default, Message)]
pub struct SetHudText {
    pub text: String,
    pub delay: Duration,
    pub duration: Duration,
    pub location: HudLocation,
}

#[derive(Resource, Default)]
struct HudState {
    current_texts: HashMap<HudLocation, Entity>,
}

#[derive(Component)]
struct RelativeTextSize {
    /// Font size as a fraction of window height (e.g. 0.05 = 5% of height)
    fraction: f32,
}

fn update_relative_text_size(
    mut resize_events: MessageReader<WindowResized>,
    mut query: Query<(&RelativeTextSize, &mut TextFont)>,
) {
    for event in resize_events.read() {
        for (rel, mut text_font) in &mut query {
            text_font.font_size = event.height * rel.fraction;
        }
    }
}
fn spawn_toast(
    mut commands: Commands,
    mut state: ResMut<HudState>,
    mut reader: MessageReader<SetHudText>,
    asset_server: Res<AssetServer>,
    window: Single<&mut Window, With<PrimaryWindow>>,
) {
    let font = asset_server.load("font.ttf");
    let fraction = 0.075;
    let font_size = window.physical_height() as f32 * fraction;
    for msg in reader.read() {
        if let Some(hud_text) = state.current_texts.get(&msg.location) {
            commands.entity(*hud_text).despawn();
            state.current_texts.remove_entry(&msg.location);
        }
        if msg.text.is_empty() {
            continue;
        }
        let show_tween = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_millis(500),
            TextColorLens {
                start: Color::srgba(0., 0., 0., 0.),
                end: Color::WHITE,
            },
        );
        let hide_tween = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_millis(500),
            TextColorLens {
                start: Color::WHITE,
                end: Color::srgba(0., 0., 0., 0.),
            },
        )
        .with_cycle_completed_event(true);

        let tween = if msg.delay == Duration::ZERO {
            show_tween.then(Delay::new(msg.duration)).then(hide_tween)
        } else {
            Delay::new(msg.delay)
                .then(show_tween)
                .then(Delay::new(msg.duration))
                .then(hide_tween)
        };

        let entity = match msg.location {
            HudLocation::InfoText => {
                commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        right: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(60.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    InfoText,
                    TextFont {
                        font: font.clone(),
                        font_size,
                        ..default()
                    },
                    RelativeTextSize { fraction },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ))
            }
            HudLocation::BottomLeft => {
                commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        left: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(40.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    TextFont {
                        font: font.clone(),
                        font_size: 64.0,
                        ..default()
                    },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ))
            }
            HudLocation::TopLeft => {
                commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        left: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(20.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    TextFont {
                        font: font.clone(),
                        font_size: font_size * 0.5,
                        ..default()
                    },
                    RelativeTextSize {
                        fraction: fraction * 0.5,
                    },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ))
            }
            HudLocation::TopRight => {
                commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        top: Val::Px(0.0),
                        right: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(60.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    TextFont {
                        font: font.clone(),
                        font_size: 72.0,
                        ..default()
                    },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ))
            }
        };
        state.current_texts.insert(msg.location, entity.id());
    }
}

/// A scrollable list of strings rendered inside a semi-transparent bordered box.
///
/// Only `visible_count` rows are shown at once, starting at `scroll_position`.
/// Mutating `scroll_position` (or `items`) marks the component changed, which
/// makes [`update_text_list`] refresh the row [`Text`]s on the next frame.
#[derive(Default, Component)]
pub struct TextList {
    pub items: Vec<String>,
    pub scroll_position: usize,
    pub visible_count: usize,
    /// Index into `items` of the currently selected row.
    pub selected: usize,
    pub controlled: bool,
}

/// Background color drawn behind the selected row.
const SELECTED_ROW_COLOR: Color = Color::srgba(1.0, 1.0, 1.0, 0.25);

/// Marks a child text entity of a [`TextList`] and records which visible row it is.
#[derive(Component)]
struct TextListRow(usize);

impl TextList {
    /// Spawns a `TextList` and its row text entities, returning the container entity.
    ///
    /// The caller can insert/override the [`Node`] on the returned entity to position it.
    pub fn spawn(
        commands: &mut Commands,
        font: Handle<Font>,
        items: Vec<String>,
        visible_count: usize,
    ) -> Entity {
        // Full-screen container that centers the content-sized box; the returned
        // entity is the box itself (the one carrying `TextList`).
        let mut box_entity = Entity::PLACEHOLDER;
        commands
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
            .with_children(|parent| {
                box_entity = parent
                    .spawn((
                        Node {
                            flex_direction: FlexDirection::Column,
                            padding: UiRect::all(Val::Px(16.0)),
                            border: UiRect::all(Val::Px(2.0)),
                            row_gap: Val::Px(4.0),
                            ..default()
                        },
                        BackgroundColor(Color::linear_rgba(0.0, 0.0, 0.0, 0.9)),
                        BorderColor::all(Color::linear_rgba(1.0, 1.0, 1.0, 0.9)),
                        TextList {
                            items,
                            visible_count,
                            controlled: true,
                            ..Default::default()
                        },
                    ))
                    .with_children(|box_node| {
                        for i in 0..visible_count {
                            box_node.spawn((
                                Text::new(""),
                                TextFont {
                                    font: font.clone(),
                                    font_size: 22.0,
                                    ..default()
                                },
                                TextColor(Color::WHITE),
                                BackgroundColor(Color::NONE),
                                TextListRow(i),
                            ));
                        }
                    })
                    .id();
            });
        box_entity
    }
}

#[derive(Message)]
pub struct TextListSelect(pub usize);

fn update_keys(
    input: Res<ButtonInput<KeyCode>>,
    mut lists: Query<&mut TextList>,
    mut writer: MessageWriter<TextListSelect>,
) {
    for mut list in &mut lists {
        if list.controlled {
            if input.just_pressed(KeyCode::ArrowUp) && list.selected > 0 {
                list.selected -= 1;
            }
            if input.just_pressed(KeyCode::ArrowDown) && list.selected < (list.items.len() - 1) {
                list.selected += 1;
            }
            if input.just_pressed(KeyCode::Enter) {
                writer.write(TextListSelect(list.selected));
            }
        }
    }
}

/// Refreshes the visible rows of every [`TextList`] whose contents or scroll changed.
///
/// Also keeps `scroll_position` in range so that `selected` stays visible, and
/// draws a background behind the selected row.
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

fn handle_tween_done(mut commands: Commands, mut reader: MessageReader<CycleCompletedEvent>) {
    for msg in reader.read() {
        info!("DESPAWN");
        commands.entity(msg.anim_entity).despawn();
    }
}

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SetHudText>()
            .insert_resource(HudState::default())
            .add_message::<TextListSelect>()
            .add_systems(
                Update,
                (
                    spawn_toast.run_if(on_message::<SetHudText>),
                    update_relative_text_size.run_if(on_message::<WindowResized>),
                    update_text_list,
                    update_keys,
                    handle_tween_done.run_if(on_message::<CycleCompletedEvent>),
                ),
            );
    }
}
