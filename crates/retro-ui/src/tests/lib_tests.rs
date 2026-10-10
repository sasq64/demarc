use super::*;

pub(crate) const SCREEN: egui::Vec2 = egui::vec2(1920.0, 1080.0);

/// A headless [`egui::Context`]: frames are run by hand on synthetic input, and
/// the texts each one painted are kept so a test can look for them and click
/// on them.
pub(crate) struct Harness {
    pub ctx: egui::Context,
    time: f64,
    texts: Vec<(String, egui::Rect)>,
}

pub(crate) fn key(key: egui::Key) -> egui::Event {
    key_with(key, egui::Modifiers::NONE)
}

/// A key press stamped with `modifiers`. That alone does not make egui think
/// they are held; [`egui::Event::ModifiersChanged`] does.
pub(crate) fn key_with(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn collect_texts(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
    match shape {
        egui::Shape::Text(text) => out.push((
            text.galley.text().to_owned(),
            text.galley.rect.translate(text.pos.to_vec2()),
        )),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect_texts(s, out)),
        _ => {}
    }
}

impl Harness {
    pub fn new() -> Self {
        Self {
            ctx: egui::Context::default(),
            time: 0.0,
            texts: Vec::new(),
        }
    }

    /// Runs one frame, a tenth of a second after the last, with `events` as
    /// its input.
    pub fn frame<R>(
        &mut self,
        events: Vec<egui::Event>,
        run: impl FnOnce(&egui::Context) -> R,
    ) -> R {
        self.time += 0.1;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        self.ctx.begin_pass(input);
        let result = run(&self.ctx);
        let mut output = self.ctx.end_pass();
        // Nothing uploads the font texture here, and dropping the delta
        // unapplied is an assertion failure in debug builds.
        output.textures_delta.clear();
        self.texts.clear();
        for clipped in &output.shapes {
            collect_texts(&clipped.shape, &mut self.texts);
        }
        result
    }

    /// Moves to `pos`, presses and releases the primary button there, then runs
    /// one more frame for what the click opened to appear. Returns what each
    /// of the four frames did. The move is a frame of its own because egui
    /// hit-tests against where the pointer was when the frame began.
    pub fn click<R>(
        &mut self,
        pos: egui::Pos2,
        mut run: impl FnMut(&egui::Context) -> R,
    ) -> Vec<R> {
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        vec![
            self.frame(vec![egui::Event::PointerMoved(pos)], &mut run),
            self.frame(vec![button(true)], &mut run),
            self.frame(vec![button(false)], &mut run),
            self.frame(vec![], &mut run),
        ]
    }

    /// Clicks the middle of the last text painted that reads exactly `text`.
    pub fn click_text<R>(&mut self, text: &str, run: impl FnMut(&egui::Context) -> R) -> Vec<R> {
        let pos = self.find(text).center();
        self.click(pos, run)
    }

    pub fn has(&self, text: &str) -> bool {
        self.texts.iter().any(|(t, _)| t == text)
    }

    pub fn find(&self, text: &str) -> egui::Rect {
        let found = self.texts.iter().rev().find(|(t, _)| t == text);
        found
            .unwrap_or_else(|| panic!("{text:?} not among {:?}", self.texts))
            .1
    }
}

/// A key is taken whatever modifiers came with it, every press of it, and
/// nothing else is.
#[test]
fn take_key_ignores_modifiers() {
    let mut h = Harness::new();
    let shifted = key_with(egui::Key::Enter, egui::Modifiers::SHIFT);
    let events = vec![key(egui::Key::Enter), shifted, key(egui::Key::Escape)];
    h.frame(events, |ctx| {
        ctx.input_mut(|i| {
            assert_eq!(take_key(i, egui::Key::Enter), 2);
            assert_eq!(take_key(i, egui::Key::Enter), 0);
            assert_eq!(take_key(i, egui::Key::Escape), 1);
        });
    });
}

#[test]
fn scale_follows_the_window_height_within_bounds() {
    let mut h = Harness::new();
    for (height, factor, zoom, want) in [
        (1600.0, 1.0, 1.0, 1.0),
        (800.0, 2.0, 1.0, 1.0),
        (10.0, 1.0, 1.0, 0.2),
        (100_000.0, 1.0, 1.0, 8.0),
        (1600.0, 1.0, 1.5, 1.5),
    ] {
        h.frame(vec![], |ctx| set_scale(ctx, height, factor, zoom));
        h.frame(vec![], |ctx| assert_eq!(ctx.pixels_per_point(), want));
    }
}

/// The app font goes in front of egui's own in both families, and the text
/// styles are the app's sizes.
#[test]
fn style_installs_the_font_and_sizes() {
    let mut h = Harness::new();
    let font = egui::FontDefinitions::default().font_data["Hack"]
        .font
        .to_vec();
    h.frame(vec![], |ctx| apply_style(ctx, font));
    h.frame(vec![], |ctx| {
        let style = ctx.global_style();
        assert_eq!(
            style.text_styles[&egui::TextStyle::Heading].size,
            HEADING_SIZE
        );
        assert_eq!(style.text_styles[&egui::TextStyle::Body].size, BODY_SIZE);
        assert_eq!(style.visuals.override_text_color, Some(TEXT_COLOR));
        ctx.fonts(|f| {
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                assert_eq!(f.definitions().families[&family][0], APP_FONT);
            }
        });
    });
}

#[test]
fn the_panel_frame_has_a_border_and_padding() {
    let frame = panel_frame();
    assert_eq!(frame.stroke.width, PANEL_BORDER);
    assert_eq!(frame.inner_margin, egui::Margin::same(PANEL_PADDING));
}
