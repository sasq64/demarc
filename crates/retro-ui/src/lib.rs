//! The app's egui UI -- the corner HUD, the searchable picker and the dialogs --
//! against a bare [`egui::Context`]. Nothing here knows what feeds the context
//! its input or paints its output, and what the user does comes back as return
//! values for the host to act on.

use std::sync::Arc;

pub mod dialog;
pub mod fuzzy_list;
pub mod hud;
pub mod picker;
pub mod shader_dialog;

pub use hud::{Hud, HudLocation};
pub use picker::{Picked, Picker};

/// Key the app font is registered under in [`egui::FontDefinitions::font_data`].
const APP_FONT: &str = "app";

pub const HEADING_SIZE: f32 = 72.0;
const BODY_SIZE: f32 = 32.0;
pub const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const MARGIN: egui::Vec2 = egui::vec2(64.0, 32.0);

/// Height of the virtual space every size in this crate is given in.
const VIRTUAL_HEIGHT: f32 = 1600.0;

/// Give `ctx` the app font and text styles.
pub fn apply_style(ctx: &egui::Context, font: Vec<u8>) {
    let mut font_defs = egui::FontDefinitions::default();
    font_defs.font_data.insert(
        APP_FONT.to_owned(),
        Arc::new(egui::FontData::from_owned(font)),
    );
    // Front of the list = primary; egui's own fonts stay behind it as fallbacks
    // for glyphs `font.ttf` happens to be missing.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        font_defs
            .families
            .entry(family)
            .or_default()
            .insert(0, APP_FONT.to_owned());
    }
    ctx.set_fonts(font_defs);

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(HEADING_SIZE),
        );
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(BODY_SIZE));
        style.visuals.override_text_color = Some(TEXT_COLOR);
    });
}

/// Scale `ctx` to a window `logical_height` points tall, so the UI keeps its
/// proportions whatever the window is sized at.
pub fn set_scale(ctx: &egui::Context, logical_height: f32, scale_factor: f32) {
    let scale = (logical_height / VIRTUAL_HEIGHT).clamp(0.2, 8.0);
    ctx.set_pixels_per_point(scale_factor * scale);
}

/// Look of the boxes the picker and the dialogs are drawn in: near-opaque black
/// behind a 2px orange border.
const PANEL_FILL: egui::Color32 = egui::Color32::from_black_alpha(230);
const PANEL_STROKE: egui::Color32 = egui::Color32::from_rgb(0xff, 0xaa, 0x7c);
const PANEL_BORDER: f32 = 2.0;
const PANEL_PADDING: i8 = 16;

pub fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL_FILL)
        .stroke(egui::Stroke::new(PANEL_BORDER, PANEL_STROKE))
        .inner_margin(egui::Margin::same(PANEL_PADDING))
}

/// Counts and removes every press of `key` among this frame's events, whatever
/// modifiers came with it, so nothing downstream acts on it.
/// [`egui::InputState::count_and_consume_key`] insists on an exact modifier
/// match instead, which is one stale modifier away from dropping the key.
pub fn take_key(i: &mut egui::InputState, key: egui::Key) -> i64 {
    let mut count = 0;
    i.events.retain(|event| {
        let hit = matches!(event, egui::Event::Key { key: k, pressed: true, .. } if *k == key);
        count += hit as i64;
        !hit
    });
    count
}

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
