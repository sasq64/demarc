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

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects every submitted line so tests can assert on it after `update()`.
    #[derive(Resource, Default)]
    struct Collected(Vec<String>);

    fn collect(mut reader: MessageReader<TextInputSubmitted>, mut out: ResMut<Collected>) {
        for msg in reader.read() {
            out.0.push(msg.text.clone());
        }
    }

    /// Build a headless app with the plugin under test plus a collector for
    /// submitted lines. No rendering/windowing plugins are needed since the
    /// systems only touch plain ECS components and messages.
    fn setup() -> App {
        let mut app = App::new();
        app.add_plugins(TextInputPlugin)
            .add_message::<KeyboardInput>()
            .init_resource::<Collected>()
            .add_systems(Update, collect.after(TextInput::on_input));
        app
    }

    /// A pressed key event. `text` mirrors what winit produces for printable keys.
    fn press(logical_key: Key, text: Option<&str>) -> KeyboardInput {
        KeyboardInput {
            key_code: KeyCode::KeyA,
            logical_key,
            state: ButtonState::Pressed,
            text: text.map(Into::into),
            repeat: false,
            window: Entity::PLACEHOLDER,
        }
    }

    fn press_char(c: &str) -> KeyboardInput {
        press(Key::Character(c.into()), Some(c))
    }

    /// Spawn a visible text input and let `was_added` create its child buffer.
    fn spawn_input(app: &mut App) -> Entity {
        let entity = app
            .world_mut()
            .spawn((
                TextInput {
                    text: String::new(),
                    showing: true,
                },
                Node::default(),
            ))
            .id();
        // First update spawns the TextBuffer child (deferred command in `was_added`).
        app.update();
        entity
    }

    /// Read back the current contents of the input's child text buffer.
    fn buffer_text(app: &mut App, parent: Entity) -> String {
        let mut query = app.world_mut().query::<(&TextBuffer, &ChildOf)>();
        for (buf, child_of) in query.iter(app.world()) {
            if child_of.parent() == parent {
                return buf.buffer.join("");
            }
        }
        panic!("no TextBuffer child found for {parent:?}");
    }

    fn buffer_pos(app: &mut App, parent: Entity) -> usize {
        let mut query = app.world_mut().query::<(&TextBuffer, &ChildOf)>();
        for (buf, child_of) in query.iter(app.world()) {
            if child_of.parent() == parent {
                return buf.pos;
            }
        }
        panic!("no TextBuffer child found for {parent:?}");
    }

    fn send(app: &mut App, keys: impl IntoIterator<Item = KeyboardInput>) {
        for key in keys {
            app.world_mut().write_message(key);
        }
        app.update();
    }

    #[test]
    fn was_added_spawns_buffer_and_cursor() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        let mut buffers = app.world_mut().query::<(&TextBuffer, &ChildOf)>();
        assert_eq!(
            buffers
                .iter(app.world())
                .filter(|(_, c)| c.parent() == entity)
                .count(),
            1,
            "expected exactly one buffer child"
        );

        let mut cursors = app.world_mut().query::<(&Cursor, &ChildOf)>();
        assert_eq!(
            cursors
                .iter(app.world())
                .filter(|(_, c)| c.parent() == entity)
                .count(),
            1,
            "expected exactly one cursor child"
        );
    }

    #[test]
    fn typing_appends_characters() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(&mut app, [press_char("h"), press_char("i")]);

        assert_eq!(buffer_text(&mut app, entity), "hi");
        assert_eq!(buffer_pos(&mut app, entity), 2);
    }

    #[test]
    fn space_inserts_a_space() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(
            &mut app,
            [
                press_char("a"),
                press(Key::Space, Some(" ")),
                press_char("b"),
            ],
        );

        assert_eq!(buffer_text(&mut app, entity), "a b");
    }

    #[test]
    fn backspace_removes_last_character() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(&mut app, [press_char("a"), press_char("b")]);
        send(&mut app, [press(Key::Backspace, None)]);

        assert_eq!(buffer_text(&mut app, entity), "a");
        assert_eq!(buffer_pos(&mut app, entity), 1);
    }

    #[test]
    fn backspace_on_empty_is_noop() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(&mut app, [press(Key::Backspace, None)]);

        assert_eq!(buffer_text(&mut app, entity), "");
        assert_eq!(buffer_pos(&mut app, entity), 0);
    }

    #[test]
    fn arrows_move_cursor_and_insert_in_the_middle() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(&mut app, [press_char("a"), press_char("c")]);
        // Move left once so the cursor sits between 'a' and 'c'.
        send(&mut app, [press(Key::ArrowLeft, None)]);
        assert_eq!(buffer_pos(&mut app, entity), 1);

        send(&mut app, [press_char("b")]);
        assert_eq!(buffer_text(&mut app, entity), "abc");
    }

    #[test]
    fn arrows_are_clamped_to_bounds() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        // Left at start stays at 0.
        send(&mut app, [press(Key::ArrowLeft, None)]);
        assert_eq!(buffer_pos(&mut app, entity), 0);

        send(&mut app, [press_char("x")]);
        // Right past the end stays at the end.
        send(&mut app, [press(Key::ArrowRight, None)]);
        assert_eq!(buffer_pos(&mut app, entity), 1);
    }

    #[test]
    fn enter_submits_and_resets() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        send(&mut app, [press_char("h"), press_char("i")]);
        send(&mut app, [press(Key::Enter, None)]);

        assert_eq!(app.world().resource::<Collected>().0, vec!["hi".to_string()]);
        assert_eq!(buffer_text(&mut app, entity), "");
        assert_eq!(buffer_pos(&mut app, entity), 0);

        let text_input = app.world().get::<TextInput>(entity).unwrap();
        assert!(!text_input.showing);
        assert!(text_input.text.is_empty());

        let node = app.world().get::<Node>(entity).unwrap();
        assert_eq!(node.display, Display::None);
    }

    #[test]
    fn input_is_ignored_when_hidden() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        // Hide it the way the rest of the app does: flip `showing` and let
        // `was_changed` propagate it to the node's display.
        app.world_mut().get_mut::<TextInput>(entity).unwrap().showing = false;
        app.update();
        assert_eq!(
            app.world().get::<Node>(entity).unwrap().display,
            Display::None
        );

        send(&mut app, [press_char("h"), press_char("i")]);

        assert_eq!(buffer_text(&mut app, entity), "");
    }

    #[test]
    fn setting_text_externally_syncs_into_buffer() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        app.world_mut().get_mut::<TextInput>(entity).unwrap().text = "preset".to_string();
        app.update();

        assert_eq!(buffer_text(&mut app, entity), "preset");
    }
}
