//! The chrome the dialogs share -- panel metrics, the widget scaling and the
//! close button -- so they look like one dialog with different contents.

use egui::Ui;

/// Sizes are in the 1600-tall virtual space [`crate::set_scale`] sets up, so
/// these are much larger than egui's defaults.
pub const TITLE_SIZE: f32 = 40.0;
/// Side of the square close button in the panel's top-right corner.
pub const CLOSE_SIZE: f32 = 40.0;
pub const LABEL_SIZE: f32 = 28.0;
pub const BODY_SIZE: f32 = 26.0;
/// Width of the editor column. Fixed, so the rows line up and the panel does not
/// resize as a combo box's text changes.
pub const WIDGET_WIDTH: f32 = 320.0;
pub const ROW_SPACING: egui::Vec2 = egui::vec2(24.0, 12.0);
/// Fraction of the screen height the field grid may take before it scrolls.
pub const GRID_HEIGHT_FRACTION: f32 = 0.85;
pub const DISABLED_COLOR: egui::Color32 = egui::Color32::from_rgb(0x80, 0x80, 0x80);
/// How much of the close button's side the painted cross spans.
const CROSS_FRACTION: f32 = 0.45;

/// Scales the widgets that size themselves from the *style* rather than from a
/// font we hand them -- checkboxes, drag values, colour swatches, buttons.
///
/// [`crate::apply_style`] only overrides the Heading and Body text styles, so
/// Button (what a [`egui::DragValue`] and a [`egui::Button`] label themselves
/// with) is left at egui's default 14pt, which is unreadably small in this app's
/// 1600-tall virtual space. The spacing has to grow with it or the widgets stay
/// letterbox-thin around the bigger text.
pub fn scale_widgets(ui: &mut Ui) {
    let style = ui.style_mut();
    style.text_styles.insert(
        egui::TextStyle::Button,
        egui::FontId::proportional(BODY_SIZE),
    );
    style.spacing.interact_size = egui::vec2(BODY_SIZE * 2.0, BODY_SIZE * 1.5);
    style.spacing.button_padding = egui::vec2(BODY_SIZE * 0.4, BODY_SIZE * 0.2);
    style.spacing.icon_width = BODY_SIZE;
    style.spacing.icon_width_inner = BODY_SIZE * 0.6;
    style.spacing.icon_spacing = BODY_SIZE * 0.3;
}

/// The x in the panel's top-right corner, which closes the dialog exactly as
/// Escape does.
///
/// Placed against `panel` -- the rect the title and fields ended up occupying --
/// because the panel is only as wide as its widest row, which is not known until
/// they are drawn. The title row has already reserved [`CLOSE_SIZE`] for it, so
/// the two cannot collide.
///
/// The cross is painted rather than written: the app's bitmap font has nothing
/// above Latin-1, so a `U+2715` glyph came out as a missing-character box.
pub fn close_button(ui: &mut Ui, panel: egui::Rect) -> bool {
    let rect = egui::Rect::from_min_size(
        egui::pos2(panel.right() - CLOSE_SIZE, panel.top()),
        egui::Vec2::splat(CLOSE_SIZE),
    );
    // `min_size`, because an empty button otherwise shrinks to its padding.
    let response = ui.put(rect, egui::Button::new("").min_size(rect.size()));
    let arm = response.rect.size().min_elem() * CROSS_FRACTION * 0.5;
    let center = response.rect.center();
    let stroke = egui::Stroke::new(
        (arm * 0.22).max(1.0),
        ui.style().interact(&response).fg_stroke.color,
    );
    let painter = ui.painter();
    for dir in [egui::vec2(arm, arm), egui::vec2(arm, -arm)] {
        painter.line_segment([center - dir, center + dir], stroke);
    }
    response.clicked()
}

fn drag<T: egui::emath::Numeric>(ui: &mut Ui, v: &mut T, min: f32, max: f32, speed: f64) -> bool {
    let range = f64::from(min)..=f64::from(max);
    ui.add(egui::DragValue::new(v).speed(speed).range(range))
        .changed()
}

/// The editor for a bare `f32` that carries its own bounds and step: a checkbox
/// for a 0/1 flag, a whole-number drag for an integral step, a fractional drag
/// otherwise.
pub fn draw_number(ui: &mut Ui, value: &mut f32, min: f32, max: f32, step: f32) -> bool {
    let whole = step >= 1.0 && step.fract() == 0.0;
    if whole && min == 0.0 && max == 1.0 {
        let mut on = *value >= 0.5;
        if !ui.checkbox(&mut on, "").changed() {
            return false;
        }
        *value = f32::from(u8::from(on));
        return true;
    }
    let speed = (f64::from(max - min).abs() / 300.0).max(f64::from(step));
    if !whole {
        return drag(ui, value, min, max, speed);
    }
    let mut whole_value = value.round() as i64;
    if !drag(ui, &mut whole_value, min, max, speed) {
        return false;
    }
    *value = whole_value as f32;
    true
}

#[cfg(test)]
#[path = "tests/dialog_tests.rs"]
mod tests;
