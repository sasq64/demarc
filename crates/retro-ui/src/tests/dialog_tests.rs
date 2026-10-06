use super::*;
use crate::tests::Harness;

/// Draws `add` in the screen's top-left corner, where a test can aim at it.
fn corner<R>(ctx: &egui::Context, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Area::new(egui::Id::new("test"))
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, add)
        .inner
}

#[test]
fn widgets_are_scaled_to_the_body_size() {
    let mut h = Harness::new();
    h.frame(vec![], |ctx| {
        corner(ctx, |ui| {
            scale_widgets(ui);
            let style = ui.style();
            assert_eq!(style.text_styles[&egui::TextStyle::Button].size, BODY_SIZE);
            assert_eq!(style.spacing.icon_width, BODY_SIZE);
            assert!(style.spacing.interact_size.y > BODY_SIZE);
        });
    });
}

/// The close button sits in the panel's top-right corner and reports a click
/// there, and only there.
#[test]
fn the_close_button_is_in_the_panels_corner() {
    let mut h = Harness::new();
    let panel = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 200.0));
    let draw = |ctx: &egui::Context| corner(ctx, |ui| close_button(ui, panel));

    assert!(!h.frame(vec![], draw));
    let inside = egui::pos2(panel.right() - CLOSE_SIZE / 2.0, CLOSE_SIZE / 2.0);
    assert!(h.click(inside, draw).contains(&true));
    assert!(!h.click(egui::pos2(20.0, 150.0), draw).contains(&true));
}

/// A whole step over 0..1 is a flag, drawn as a checkbox.
#[test]
fn a_zero_one_number_is_a_checkbox() {
    let mut h = Harness::new();
    let mut value = 0.0;
    let draw = |ctx: &egui::Context, value: &mut f32| {
        corner(ctx, |ui| draw_number(ui, value, 0.0, 1.0, 1.0))
    };

    assert!(!h.frame(vec![], |ctx| draw(ctx, &mut value)));
    let at = egui::pos2(8.0, 8.0);
    assert!(h.click(at, |ctx| draw(ctx, &mut value)).contains(&true));
    assert_eq!(value, 1.0);
    assert!(h.click(at, |ctx| draw(ctx, &mut value)).contains(&true));
    assert_eq!(value, 0.0);
}

/// Drags the number editor `distance` points to the right and returns what
/// the value ended up as, and whether any frame reported a change.
fn dragged(start: f32, min: f32, max: f32, step: f32, distance: f32) -> (f32, bool) {
    let mut h = Harness::new();
    let mut value = start;
    let mut draw =
        |ctx: &egui::Context| corner(ctx, |ui| draw_number(ui, &mut value, min, max, step));
    let from = egui::pos2(10.0, 8.0);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    let mut changed = h.frame(vec![], &mut draw);
    changed |= h.frame(vec![egui::Event::PointerMoved(from)], &mut draw);
    changed |= h.frame(vec![button(from, true)], &mut draw);
    let mut pos = from;
    for _ in 0..10 {
        pos.x += distance / 10.0;
        changed |= h.frame(vec![egui::Event::PointerMoved(pos)], &mut draw);
    }
    changed |= h.frame(vec![button(pos, false)], &mut draw);
    (value, changed)
}

/// A whole step drags in whole numbers.
#[test]
fn a_whole_step_drags_whole_numbers() {
    let (value, changed) = dragged(2.0, 0.0, 100.0, 1.0, 30.0);
    assert!(changed);
    assert!(value > 2.0 && value < 100.0, "{value}");
    assert_eq!(value.fract(), 0.0);
}

#[test]
fn a_fractional_step_drags_fractions() {
    let (value, changed) = dragged(0.5, 0.0, 1.0, 0.01, 20.0);
    assert!(changed);
    assert!(value > 0.5 && value < 1.0, "{value}");
}

/// A drag cannot leave the parameter's range.
#[test]
fn a_drag_stops_at_the_range() {
    let (value, _) = dragged(0.5, 0.0, 1.0, 0.01, 800.0);
    assert_eq!(value, 1.0);
    let (value, _) = dragged(5.0, 0.0, 10.0, 1.0, -800.0);
    assert_eq!(value, 0.0);
}
