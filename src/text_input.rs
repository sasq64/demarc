#![allow(clippy::type_complexity)]
use bevy::{
    input::{
        ButtonState,
        keyboard::{Key, KeyboardInput},
    },
    prelude::*,
    text::TextLayoutInfo,
};

/// A line of text was input by the user
#[derive(Message, Debug, Clone)]
pub struct TextInputSubmitted {
    pub text: String,
}

#[derive(Debug, Default, Component)]
pub struct TextInput {
    pub text: String,
    pub showing: bool,
}

#[derive(Debug, Default, Component)]
struct TextBuffer {
    buffer: Vec<String>,
    pos: usize,
}
#[derive(Debug, Default, Component)]
struct Cursor;

impl TextInput {
    fn was_added(mut commands: Commands, query: Query<(&TextInput, Entity), Added<TextInput>>) {
        let cursor_x = 50.0;
        let line_height = 20.0;
        for (text_input, entity) in query {
            commands.entity(entity).with_children(|parent| {
                parent.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        width: Val::Auto,
                        ..default()
                    },
                    Text::new(&text_input.text),
                    TextColor(Color::linear_rgb(0.5, 0.5, 1.0)),
                    TextLayout {
                        justify: Justify::Left,
                        linebreak: LineBreak::NoWrap,
                    },
                    TextBuffer::default(),
                ));
                parent.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(cursor_x),
                        top: Val::Px(0.0),
                        width: Val::Px(2.0),
                        height: Val::Px(line_height),
                        ..default()
                    },
                    BackgroundColor(Color::WHITE),
                    ZIndex(1),
                    Cursor,
                ));
            });
        }
    }
    fn was_changed(
        query: Query<(&mut Node, &TextInput, Entity), Changed<TextInput>>,
        mut buffer: Query<(&mut Text, &mut TextBuffer, &ChildOf)>,
    ) {
        for (mut node, text_input, entity) in query {
            for (mut text, mut b, child_of) in &mut buffer {
                if entity != child_of.parent() {
                    continue;
                }
                let old_text = b.buffer.join("");
                if old_text != text_input.text {
                    b.buffer = text_input.text.chars().map(|c| c.to_string()).collect();
                    text.0 = text_input.text.clone();
                    b.pos = text_input.text.len();
                }
                node.display = if text_input.showing {
                    Display::Flex
                } else {
                    Display::None
                };
            }
        }
    }
    fn on_input(
        mut messages: MessageReader<KeyboardInput>,
        query: Query<(&mut Node, &mut TextInput, Entity)>,
        mut buffer: Query<(&mut Text, &mut TextBuffer, &ChildOf)>,
        mut submitted: MessageWriter<TextInputSubmitted>,
    ) {
        for (mut node, mut text_input, entity) in query {
            if node.display == Display::None {
                messages.clear();
                continue;
            }
            for (mut text, mut b, child_of) in &mut buffer {
                if entity != child_of.parent() {
                    continue;
                }
                for key in messages.read() {
                    if matches!(key.state, ButtonState::Pressed) {
                        let pos = b.pos;
                        info!("{:?}", key);
                        match &key.logical_key {
                            Key::Backspace => {
                                if pos > 0 {
                                    b.buffer.remove(pos - 1);
                                    b.pos -= 1;
                                }
                            }
                            Key::Space => {
                                b.buffer.insert(pos, " ".to_string());
                                b.pos += 1;
                            }
                            Key::ArrowLeft => {
                                if pos > 0 {
                                    b.pos -= 1;
                                }
                            }
                            Key::ArrowRight => {
                                if pos < b.buffer.len() {
                                    b.pos += 1;
                                }
                            }
                            Key::Enter => {
                                node.display = Display::None;
                                text_input.showing = false;
                                submitted.write(TextInputSubmitted {
                                    text: b.buffer.join(""),
                                });
                                b.buffer.clear();
                                b.pos = 0;
                                text_input.text.clear();
                            }
                            Key::Tab => {}
                            _ => {
                                if let Some(text) = &key.text {
                                    info!("TEXT: {text}");
                                    b.buffer.insert(pos, text.to_string());
                                    b.pos += 1;
                                }
                            }
                        };
                    }
                }
                text.0 = b.buffer.join("");
                let ti = text_input.bypass_change_detection();
                ti.text = text.0.clone();
            }
        }
    }
    fn update_cursor(
        buffer: Query<(&TextLayoutInfo, &TextBuffer, &ChildOf), Changed<TextLayoutInfo>>,
        mut cursor: Query<(&mut Node, &ChildOf), With<Cursor>>,
    ) {
        for (layout, buffer, child_of) in buffer {
            let x = if layout.glyphs.len() > buffer.pos {
                layout.glyphs[buffer.pos].position.x / layout.scale_factor - 5.0
            } else {
                layout.size.x
            };
            for (mut node, child_of2) in cursor.iter_mut() {
                if child_of.parent() == child_of2.parent() {
                    node.left = Val::Px(x);
                }
            }
        }
    }
}

pub struct TextInputPlugin;

impl Plugin for TextInputPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<TextInputSubmitted>()
            .add_systems(PostUpdate, TextInput::update_cursor)
            .add_systems(
                Update,
                (
                    TextInput::was_added,
                    TextInput::was_changed,
                    TextInput::on_input.run_if(on_message::<KeyboardInput>),
                ),
            );
    }
}
