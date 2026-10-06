use super::*;

/// Lays the picker's list box out on a `size`-sized screen for `frames` frames
/// and returns the height of the box on each of them. The list lives in an
/// anchored [`egui::Area`] with a panel above it, as [`render_list`] draws it.
fn list_heights(size: egui::Vec2, items: usize, frames: usize) -> Vec<f32> {
    let ctx = egui::Context::default();
    let ids: Vec<usize> = (0..items).collect();
    let mut heights = Vec::new();
    for _ in 0..frames {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut height = 0.0;
        ctx.begin_pass(input);
        {
            let ctx = &ctx;
            egui::Area::new(egui::Id::new("fuzzy_list"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_width(size.y);
                    panel_frame().show(ui, |ui| {
                        ui.label(egui::RichText::new("query").size(QUERY_SIZE));
                    });
                    let out = scroll_area(ui, items.saturating_sub(1), 0.0, &ids, |_, _, _| {});
                    height = out.inner_rect.height();
                });
        }
        // Nothing uploads the font texture here, and dropping the delta
        // unapplied is an assertion failure in debug builds.
        ctx.end_pass().textures_delta.clear();
        heights.push(height);
    }
    heights
}

/// The box is its full height from the very first frame, however little the
/// rest of the picker asks for: the area's stale size must not cap it.
#[test]
fn list_box_is_full_height_at_once() {
    let size = egui::vec2(1920.0, 1080.0);
    let want = (size.y * LIST_HEIGHT_FRACTION / ROW_HEIGHT).floor() * ROW_HEIGHT;
    for h in list_heights(size, 200, 3) {
        assert!(
            (h - want).abs() < 1.0,
            "list box {h} points tall, want {want}"
        );
    }
}

use crate::fuzzy_list::{AllWordsSource, WordsIconSource};
use crate::tests::{Harness, key, key_with};

const FRUIT: &[&str] = &["apple", "banana", "cherry", "date", "elderberry"];

fn fruit() -> ListSource<()> {
    Arc::new(AllWordsSource::new(
        FRUIT.iter().map(|s| (*s).to_owned()).collect(),
    ))
}

/// A source with more rows than fit on screen, and something to say about
/// each of them.
struct Numbers;

impl FuzzySource<u32> for Numbers {
    fn search(&self, _query: &str, _limit: usize) -> Vec<usize> {
        (0..200).collect()
    }
    fn get_text(&self, id: usize) -> String {
        format!("row {id}")
    }
    fn get_info(&self, id: usize) -> String {
        format!("about {id}")
    }
    fn get_data(&self, id: usize) -> Option<u32> {
        Some(id as u32 * 2)
    }
}

/// One frame of `picker` with the plain row painter.
fn step<T: 'static>(
    h: &mut Harness,
    picker: &mut Picker<T>,
    events: Vec<egui::Event>,
) -> Option<Picked<T>> {
    h.frame(events, |ctx| {
        picker.show(ctx, |ui, rect, source, id| {
            let job = egui::text::LayoutJob::simple_singleline(
                source.get_text(id),
                egui::FontId::proportional(ROW_SIZE),
                TEXT_COLOR,
            );
            draw_row(ui, rect, job, source.get_icon(id));
        })
    })
}

/// An opened picker, two frames in so the search box has its focus.
fn opened<T: 'static>(h: &mut Harness, source: ListSource<T>) -> Picker<T> {
    let mut picker = Picker::default();
    picker.open(1, source, None, None, "Title");
    step(h, &mut picker, vec![]);
    step(h, &mut picker, vec![]);
    picker
}

#[test]
fn a_closed_picker_draws_nothing() {
    let mut h = Harness::new();
    let mut picker = Picker::<()>::default();
    assert!(!picker.is_open());
    assert!(step(&mut h, &mut picker, vec![]).is_none());
    assert_eq!(picker.query(0), None);
    assert_eq!(picker.selected_item(0), None);
}

/// The title, the rows and nothing of the info box, which a source with
/// nothing to say leaves hidden.
#[test]
fn an_open_picker_shows_its_title_and_rows() {
    let mut h = Harness::new();
    let picker = opened(&mut h, fruit());
    assert!(picker.is_open());
    assert!(h.has("Title"));
    for name in FRUIT {
        assert!(h.has(name), "{name} missing");
    }
    assert_eq!(picker.query(1), Some(""));
    assert_eq!(picker.query(2), None, "another list's id");
    assert_eq!(picker.selected_item(1), Some(0));
    assert_eq!(picker.selected_item(2), None);
}

/// The arrows move the highlight a row at a time, every press counted, and
/// stop at both ends of the list.
#[test]
fn arrows_move_the_selection_and_clamp() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, fruit());
    let down = || key(egui::Key::ArrowDown);

    step(&mut h, &mut picker, vec![down(), down()]);
    assert_eq!(picker.selected_item(1), Some(2));
    step(&mut h, &mut picker, vec![key(egui::Key::ArrowUp)]);
    assert_eq!(picker.selected_item(1), Some(1));
    step(&mut h, &mut picker, vec![down(); 20]);
    assert_eq!(picker.selected_item(1), Some(FRUIT.len() - 1));
    step(&mut h, &mut picker, vec![key(egui::Key::ArrowUp); 20]);
    assert_eq!(picker.selected_item(1), Some(0));
}

/// PageDown moves one screenful, and the list scrolls to keep the row in view.
#[test]
fn page_keys_move_a_screenful() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, Arc::new(Numbers) as ListSource<u32>);
    let page = visible_rows(&h.ctx) as usize;
    assert!(h.has("row 0") && !h.has("row 150"));
    assert!(h.has("about 0"), "the info box describes the selection");

    step(&mut h, &mut picker, vec![key(egui::Key::PageDown); 2]);
    assert_eq!(picker.selected_item(1), Some(page * 2));
    step(&mut h, &mut picker, vec![]);
    assert!(h.has(&format!("row {}", page * 2)));
    assert!(h.has(&format!("about {}", page * 2)));
    assert!(!h.has("row 0"), "scrolled out of view");

    step(&mut h, &mut picker, vec![key(egui::Key::PageUp)]);
    assert_eq!(picker.selected_item(1), Some(page));
}

/// Enter reports the highlighted row with the source's record, and closes.
#[test]
fn enter_picks_the_highlighted_row() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, Arc::new(Numbers) as ListSource<u32>);
    step(&mut h, &mut picker, vec![key(egui::Key::ArrowDown); 3]);
    let picked = step(&mut h, &mut picker, vec![key(egui::Key::Enter)]).expect("a pick");
    assert_eq!(picked.id, 1);
    assert_eq!(picked.item, 3);
    assert_eq!(picked.text, "row 3");
    assert_eq!(picked.data, Some(6));
    assert!(!picked.alt);
    assert!(!picker.is_open());
}

#[test]
fn shift_enter_is_the_alternative_pick() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, fruit());
    let shift = egui::Modifiers::SHIFT;
    let events = vec![
        egui::Event::ModifiersChanged(shift),
        key_with(egui::Key::Enter, shift),
    ];
    let picked = step(&mut h, &mut picker, events).expect("a pick");
    assert!(picked.alt);
    assert_eq!(picked.text, "apple");
    assert_eq!(picked.data, None);
}

#[test]
fn escape_closes_without_a_pick() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, fruit());
    assert!(step(&mut h, &mut picker, vec![key(egui::Key::Escape)]).is_none());
    assert!(!picker.is_open());
}

/// Typing lands in the search box, filters the rows and starts the selection
/// at the top of what is left; Enter on an empty result picks nothing.
#[test]
fn typing_filters_the_list() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, fruit());
    step(&mut h, &mut picker, vec![key(egui::Key::ArrowDown)]);

    step(
        &mut h,
        &mut picker,
        vec![egui::Event::Text("rr".to_owned())],
    );
    assert_eq!(picker.query(1), Some("rr"));
    step(&mut h, &mut picker, vec![]);
    assert_eq!(
        picker.selected_item(1),
        Some(2),
        "cherry is the first match"
    );
    step(&mut h, &mut picker, vec![]);
    assert!(h.has("cherry") && h.has("elderberry") && !h.has("apple"));

    step(
        &mut h,
        &mut picker,
        vec![egui::Event::Text("zz".to_owned())],
    );
    step(&mut h, &mut picker, vec![]);
    assert_eq!(picker.selected_item(1), None);
    assert!(step(&mut h, &mut picker, vec![key(egui::Key::Enter)]).is_none());
    assert!(picker.is_open());
}

/// Re-opening the same list restores the search text; a prompt replaces it, a
/// wanted row is highlighted, and another list's id starts over.
#[test]
fn reopening_restores_or_resets() {
    let mut h = Harness::new();
    let mut picker = opened(&mut h, fruit());
    step(&mut h, &mut picker, vec![egui::Event::Text("e".to_owned())]);
    step(&mut h, &mut picker, vec![key(egui::Key::Escape)]);

    picker.open(1, fruit(), None, Some(3), "");
    step(&mut h, &mut picker, vec![]);
    assert_eq!(picker.query(1), Some("e"));
    assert_eq!(picker.selected_item(1), Some(3), "date, among the matches");
    step(&mut h, &mut picker, vec![]);
    assert!(!h.has("Title"), "an empty title is no line at all");

    picker.open(1, fruit(), Some("ban"), None, "");
    step(&mut h, &mut picker, vec![]);
    assert_eq!(picker.query(1), Some("ban"));
    assert_eq!(picker.selected_item(1), Some(1));

    picker.open(2, fruit(), None, Some(99), "");
    step(&mut h, &mut picker, vec![]);
    assert_eq!(picker.query(2), Some(""));
    assert_eq!(picker.selected_item(2), Some(0), "an absent row is ignored");
}

/// Both kinds of icon are drawn: a glyph as text, an image as a texture
/// uploaded once per context.
#[test]
fn rows_draw_their_icons() {
    add_list_icon(7, egui::ColorImage::filled([2, 2], egui::Color32::WHITE));
    let source: ListSource<()> = Arc::new(WordsIconSource::new(vec![
        ("glyph".to_owned(), ListIcon::Glyph('#', 0x12_34_56)),
        ("image".to_owned(), ListIcon::Image(7)),
        ("unregistered".to_owned(), ListIcon::Image(u32::MAX)),
    ]));
    let mut h = Harness::new();
    opened(&mut h, source);
    assert!(h.has("#") && h.has("glyph") && h.has("image"));
    assert!(list_icon_texture(&h.ctx, 7).is_some());
    assert_eq!(list_icon_texture(&h.ctx, 7), list_icon_texture(&h.ctx, 7));
    assert!(list_icon_texture(&h.ctx, u32::MAX).is_none());
}
