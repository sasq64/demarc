#![allow(clippy::type_complexity)]
use std::collections::HashMap;
use std::time::Duration;

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;

use bevy::window::{PrimaryWindow, WindowResized};

#[derive(Component)]
pub struct InfoText;

/// Drives a HUD toast's text alpha through a fade-in / hold / fade-out timeline,
/// then despawns the entity. Replaces the old `bevy_tweening` animation.
#[derive(Component)]
struct HudFade {
    elapsed: Duration,
    /// Wait before fading in.
    delay: Duration,
    /// Fade alpha 0 -> 1.
    fade_in: Duration,
    /// Stay fully visible.
    hold: Duration,
    /// Fade alpha 1 -> 0.
    fade_out: Duration,
}

impl HudFade {
    fn new(delay: Duration, hold: Duration) -> Self {
        Self {
            elapsed: Duration::ZERO,
            delay,
            fade_in: Duration::from_millis(500),
            hold,
            fade_out: Duration::from_millis(500),
        }
    }
}

/// Quadratic in/out easing, matching the old `EaseFunction::QuadraticInOut`.
fn ease_quad_in_out(t: f32) -> f32 {
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

#[derive(Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
    Error,
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
            let size = event.height * rel.fraction;
            text_font.font_size = FontSize::Px(size);
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
    let font: FontSource = font.into();
    let fraction = 0.05;
    let font_size = window.height() * fraction;
    for msg in reader.read() {
        if let Some(hud_text) = state.current_texts.get(&msg.location) {
            commands.entity(*hud_text).despawn();
            state.current_texts.remove_entry(&msg.location);
        }
        if msg.text.is_empty() {
            continue;
        }
        let fade = || HudFade::new(msg.delay, msg.duration);

        let entity = match msg.location {
            HudLocation::InfoText => {
                commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        right: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(40.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    InfoText,
                    TextFont {
                        font: font.clone(),
                        font_size: FontSize::Px(font_size),
                        ..default()
                    },
                    RelativeTextSize { fraction },
                    TextColor(Color::srgba(1.0, 1.0, 1.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    fade(),
                ))
            }
            HudLocation::BottomLeft => commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(0.0),
                    left: Val::Px(0.0),
                    margin: UiRect::all(Val::Px(40.0)),
                    ..default()
                },
                Text::new(&msg.text),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(64.0),
                    ..default()
                },
                TextColor(Color::srgba(1.0, 1.0, 1.0, 0.0)),
                TextLayout {
                    justify: Justify::Right,
                    linebreak: LineBreak::WordBoundary,
                },
                fade(),
            )),
            HudLocation::TopLeft => commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    left: Val::Px(0.0),
                    margin: UiRect::all(Val::Px(20.0)),
                    ..default()
                },
                Text::new(&msg.text),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(font_size * 0.7),
                    ..default()
                },
                RelativeTextSize {
                    fraction: fraction * 0.7,
                },
                TextColor(Color::srgba(1.0, 1.0, 1.0, 0.0)),
                TextLayout {
                    justify: Justify::Right,
                    linebreak: LineBreak::WordBoundary,
                },
                fade(),
            )),
            HudLocation::TopRight => commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(0.0),
                    right: Val::Px(0.0),
                    margin: UiRect::all(Val::Px(60.0)),
                    ..default()
                },
                Text::new(&msg.text),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(72.0),
                    ..default()
                },
                TextColor(Color::srgba(1.0, 1.0, 1.0, 0.0)),
                TextLayout {
                    justify: Justify::Right,
                    linebreak: LineBreak::WordBoundary,
                },
                fade(),
            )),
            HudLocation::Error => commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    bottom: Val::Px(0.0),
                    left: Val::Px(0.0),
                    margin: UiRect::all(Val::Px(40.0)),
                    ..default()
                },
                Text::new(&msg.text),
                TextFont {
                    font: font.clone(),
                    font_size: FontSize::Px(32.0),
                    ..default()
                },
                TextColor(Color::srgba(1.0, 0.0, 0.0, 0.0)),
                TextLayout {
                    justify: Justify::Right,
                    linebreak: LineBreak::WordBoundary,
                },
                fade(),
            )),
        };
        state.current_texts.insert(msg.location, entity.id());
    }
}

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

/// Advances each [`HudFade`], updating the text alpha, and despawns the toast
/// once the fade-out completes.
fn drive_hud_fades(
    time: Res<Time>,
    mut commands: Commands,
    mut state: ResMut<HudState>,
    mut query: Query<(Entity, &mut HudFade, &mut TextColor)>,
) {
    for (entity, mut fade, mut color) in &mut query {
        fade.elapsed += time.delta();

        let e = fade.elapsed.as_secs_f32();
        let d = fade.delay.as_secs_f32();
        let fi = fade.fade_in.as_secs_f32();
        let h = fade.hold.as_secs_f32();
        let fo = fade.fade_out.as_secs_f32();

        let alpha = if e < d {
            0.0
        } else if e < d + fi {
            ease_quad_in_out((e - d) / fi)
        } else if e < d + fi + h {
            1.0
        } else if e < d + fi + h + fo {
            1.0 - ease_quad_in_out((e - (d + fi + h)) / fo)
        } else {
            state.current_texts.retain(|_, ent| *ent != entity);
            commands.entity(entity).despawn();
            continue;
        };

        let mut col = color.0;
        col.set_alpha(alpha);
        color.0 = col;
        //color.0 = Color::srgba(1.0, 1.0, 1.0, alpha);
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
                    TextList::update_text_list,
                    TextList::update_keys.run_if(on_message::<KeyboardInput>),
                    drive_hud_fades,
                ),
            );
    }
}
