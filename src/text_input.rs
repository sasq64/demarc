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
    #[allow(dead_code)]
    pub text: String,
}

#[derive(Debug, Default, Component)]
pub struct TextInput {
    pub text: String,
    pub showing: bool,
    /// When true, the Enter key is left untouched for another system to handle
    /// instead of submitting and clearing the input. Used when the input is
    /// embedded in a larger widget (e.g. `FuzzyList`) that owns Enter itself.
    pub ignore_enter: bool,
    /// Face to render the text with. Leaving this at the default handle picks
    /// Bevy's built-in `FiraMono-subset`, which only covers ASCII — anything
    /// beyond that (åäö, é, …) silently renders as nothing. Callers should
    /// hand over the app font so non-ASCII input is visible.
    pub font: Handle<Font>,
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
        let line_height = 20.0;
        for (text_input, entity) in query {
            commands.entity(entity).with_children(|parent| {
                // The text flows normally (not absolute) so the parent's padding
                // insets it equally on every side — the box centers it. Reserve a
                // line of height so an empty field keeps the same size (and stays
                // centered) as a filled one instead of collapsing to zero.
                parent.spawn((
                    Node {
                        width: Val::Auto,
                        min_height: Val::Px(line_height + 4.0),
                        ..default()
                    },
                    Text::new(&text_input.text),
                    TextFont {
                        font: text_input.font.clone().into(),
                        font_size: FontSize::Px(line_height),
                        ..default()
                    },
                    TextColor(Color::linear_rgb(0.5, 0.5, 1.0)),
                    TextLayout {
                        justify: Justify::Left,
                        linebreak: LineBreak::NoWrap,
                    },
                    // Seed the buffer from the initial text so a pre-filled
                    // input (e.g. a restored `FuzzyList` search) survives the
                    // first keystroke — `on_input` rewrites `TextInput::text`
                    // from this buffer, so an empty buffer would clear it.
                    TextBuffer {
                        buffer: text_input.text.chars().map(|c| c.to_string()).collect(),
                        pos: text_input.text.chars().count(),
                    },
                ));
                // The cursor overlays the text. It's an absolute sibling (a
                // direct child of the box, not of the `Text` node — `Text`
                // treats children as spans), positioned each frame in
                // `update_cursor` by the box's padding plus the caret's glyph x.
                parent.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(0.0),
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
                    // Chars, not bytes: `buffer` holds one entry per char, so a
                    // byte length would put `pos` past its end for non-ASCII
                    // text and panic on the next insert.
                    b.pos = b.buffer.len();
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
        // Drain the reader once, up front. The same keystrokes are applied to
        // whichever input is currently visible. Reading here rather than inside
        // the entity loop is essential once more than one `TextInput` exists
        // (e.g. a `FuzzyList` search box plus the hidden standalone input): a
        // `MessageReader` shares a single cursor, so the first entity to call
        // `read()` would otherwise consume every event and leave the rest with
        // nothing.
        let keys: Vec<KeyboardInput> = messages.read().cloned().collect();

        for (mut node, mut text_input, entity) in query {
            if node.display == Display::None {
                continue;
            }
            for (mut text, mut b, child_of) in &mut buffer {
                if entity != child_of.parent() {
                    continue;
                }
                for key in &keys {
                    if matches!(key.state, ButtonState::Pressed) {
                        let pos = b.pos;
                        trace!("{:?}", key);
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
                                // When embedded, let the owning widget handle
                                // Enter (selection) rather than submitting here.
                                if !text_input.ignore_enter {
                                    node.display = Display::None;
                                    text_input.showing = false;
                                    submitted.write(TextInputSubmitted {
                                        text: b.buffer.join(""),
                                    });
                                    b.buffer.clear();
                                    b.pos = 0;
                                    text_input.text.clear();
                                }
                            }
                            Key::Tab => {}
                            Key::Escape => {
                                b.buffer.clear();
                                b.pos = 0;
                            }
                            _ => {
                                if let Some(text) = &key.text {
                                    trace!("TEXT: {text}");
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
        boxes: Query<&ComputedNode>,
        mut cursor: Query<(&mut Node, &ChildOf), With<Cursor>>,
    ) {
        for (layout, b, child_of) in buffer {
            // The text and cursor are absolute siblings inside the input box, but
            // the in-flow text sits at the box's content origin (inset by its
            // padding) while an absolute cursor sits at the padding-box edge.
            // Add the box's resolved padding so the cursor lines up with the text.
            let (pad_left, pad_top) = boxes
                .get(child_of.parent())
                .map(|c| {
                    let s = c.inverse_scale_factor();
                    (c.padding().min_inset.x * s, c.padding().min_inset.y * s)
                })
                .unwrap_or((0.0, 0.0));

            let x = if layout.glyphs.len() > b.pos {
                layout.glyphs[b.pos].position.x / layout.scale_factor - 5.0
            } else {
                layout.size.x
            };
            for (mut node, cursor_child_of) in cursor.iter_mut() {
                if child_of.parent() == cursor_child_of.parent() {
                    node.left = Val::Px(pad_left + x);
                    node.top = Val::Px(pad_top);
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
                    showing: true,
                    ..default()
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

        assert_eq!(
            app.world().resource::<Collected>().0,
            vec!["hi".to_string()]
        );
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
        app.world_mut()
            .get_mut::<TextInput>(entity)
            .unwrap()
            .showing = false;
        app.update();
        assert_eq!(
            app.world().get::<Node>(entity).unwrap().display,
            Display::None
        );

        send(&mut app, [press_char("h"), press_char("i")]);

        assert_eq!(buffer_text(&mut app, entity), "");
    }

    /// A hidden input must not swallow keystrokes meant for a visible one.
    /// This is the multi-`TextInput` case a `FuzzyList` creates (its visible
    /// search box coexists with the hidden standalone input): a shared
    /// `MessageReader` cursor used to let whichever entity ran first drain the
    /// events, starving the rest.
    #[test]
    fn visible_input_reads_keys_alongside_a_hidden_one() {
        let mut app = setup();

        // Hidden input.
        app.world_mut().spawn((
            TextInput::default(),
            Node {
                display: Display::None,
                ..default()
            },
        ));
        // Visible input.
        let visible = spawn_input(&mut app);

        send(&mut app, [press_char("h"), press_char("i")]);

        assert_eq!(buffer_text(&mut app, visible), "hi");
    }

    #[test]
    fn setting_text_externally_syncs_into_buffer() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        app.world_mut().get_mut::<TextInput>(entity).unwrap().text = "preset".to_string();
        app.update();

        assert_eq!(buffer_text(&mut app, entity), "preset");
    }

    /// `buffer` holds one entry per char, so syncing external text must place
    /// the caret by char count. A byte length would leave `pos` past the end of
    /// a non-ASCII preset and panic on the next insert.
    #[test]
    fn setting_non_ascii_text_externally_leaves_a_usable_caret() {
        let mut app = setup();
        let entity = spawn_input(&mut app);

        app.world_mut().get_mut::<TextInput>(entity).unwrap().text = "åäö".to_string();
        app.update();

        assert_eq!(buffer_text(&mut app, entity), "åäö");
        assert_eq!(buffer_pos(&mut app, entity), 3);

        send(&mut app, [press_char("é")]);
        assert_eq!(buffer_text(&mut app, entity), "åäöé");

        send(&mut app, [press(Key::Backspace, None)]);
        assert_eq!(buffer_text(&mut app, entity), "åäö");
    }
}
