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
pub mod theme;

pub use hud::{Hud, HudLocation};
pub use picker::{Picked, Picker, TitleClick};
pub use theme::{
    ShaderBg, ShaderPainter, Theme, paint_screen, set_shader_painter, set_theme, theme,
};

/// Key the app font is registered under in [`egui::FontDefinitions::font_data`].
const APP_FONT: &str = "app";

pub const HEADING_SIZE: f32 = 72.0;
const BODY_SIZE: f32 = 32.0;
const MARGIN: egui::Vec2 = egui::vec2(64.0, 32.0);

/// Height of the virtual space every size in this crate is given in.
const VIRTUAL_HEIGHT: f32 = 1600.0;

/// Give `ctx` the app font, text styles and [`Theme::DEFAULT`].
pub fn apply_style(ctx: &egui::Context, font: Vec<u8>) {
    ctx.data_mut(|d| d.insert_temp(app_font_id(), Arc::new(egui::FontData::from_owned(font))));
    set_fonts(ctx, [None, None, None]);

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(HEADING_SIZE),
        );
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(BODY_SIZE));
    });
    set_theme(ctx, Theme::DEFAULT);
}

const PICKER_FAMILY: &str = "picker";
const HUD_FAMILY: &str = "hud";

fn app_font_id() -> egui::Id {
    egui::Id::new("retro_ui_app_font")
}

/// Puts the theme's UI, picker and HUD fonts (see [`Theme::fonts`]) in front of
/// the app font, which stays behind them as a fallback for the glyphs they lack.
pub fn set_fonts(ctx: &egui::Context, fonts: [Option<Vec<u8>>; 3]) {
    use egui::FontFamily;
    let Some(app) = ctx.data(|d| d.get_temp::<Arc<egui::FontData>>(app_font_id())) else {
        return;
    };
    let mut defs = egui::FontDefinitions::default();
    defs.font_data.insert(APP_FONT.to_owned(), app);
    for (name, font) in ["ui", PICKER_FAMILY, HUD_FAMILY].into_iter().zip(fonts) {
        if let Some(font) = font {
            let data = Arc::new(egui::FontData::from_owned(font));
            defs.font_data.insert(name.to_owned(), data);
        }
    }
    let chain = |name: &str, family: &FontFamily| -> Vec<String> {
        [name, "ui", APP_FONT]
            .into_iter()
            .enumerate()
            .filter(|&(i, n)| !(i == 1 && name == "ui") && defs.font_data.contains_key(n))
            .map(|(_, n)| n)
            .map(str::to_owned)
            .chain(defs.families[family].iter().cloned())
            .collect()
    };
    let families = [
        (
            FontFamily::Proportional,
            chain("ui", &FontFamily::Proportional),
        ),
        (FontFamily::Monospace, chain("ui", &FontFamily::Monospace)),
        (
            FontFamily::Name(PICKER_FAMILY.into()),
            chain(PICKER_FAMILY, &FontFamily::Proportional),
        ),
        (
            FontFamily::Name(HUD_FAMILY.into()),
            chain(HUD_FAMILY, &FontFamily::Proportional),
        ),
    ];
    defs.families.extend(families);
    ctx.set_fonts(defs);
}

fn family(ctx: &egui::Context, name: &str) -> egui::FontFamily {
    if ctx
        .data(|d| d.get_temp::<Arc<egui::FontData>>(app_font_id()))
        .is_some()
    {
        egui::FontFamily::Name(name.into())
    } else {
        egui::FontFamily::Proportional
    }
}

pub(crate) fn picker_font(ctx: &egui::Context, size: f32) -> egui::FontId {
    egui::FontId::new(size, family(ctx, PICKER_FAMILY))
}

pub(crate) fn hud_font(ctx: &egui::Context, size: f32) -> egui::FontId {
    egui::FontId::new(size, family(ctx, HUD_FAMILY))
}

/// Scale `ctx` to a window `logical_height` points tall, so the UI keeps its
/// proportions whatever the window is sized at. `zoom` enlarges it on top.
pub fn set_scale(ctx: &egui::Context, logical_height: f32, scale_factor: f32, zoom: f32) {
    let scale = (logical_height / VIRTUAL_HEIGHT).clamp(0.2, 8.0);
    ctx.set_pixels_per_point(scale_factor * scale * zoom);
}

/// Draws `add_contents` in the box the picker and the dialogs are drawn in.
pub fn show_panel<R>(
    ui: &mut egui::Ui,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> egui::InnerResponse<R> {
    theme(ui.ctx()).panel.show(ui, add_contents)
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
