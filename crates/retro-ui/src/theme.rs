//! Every colour, stroke and fill the UI is drawn with, in one place. Layout and
//! sizes stay with the code that lays things out.

use std::sync::Arc;

use egui::{Color32, Stroke};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn gray(l: u8) -> Color32 {
    Color32::from_gray(l)
}

const fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke { width, color }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub text: Color32,
    pub dim_text: Color32,
    pub panel: Panel,
    /// Combo box drop-downs and other popups.
    pub popup: Panel,
    /// Background of text fields and drag values.
    pub field_fill: Color32,
    pub selection: Selection,
    pub widgets: Widgets,
    pub picker: Picker,
    pub hud: Hud,
}

/// The box the picker and the dialogs are drawn in.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub fill: Color32,
    pub border: Stroke,
    pub padding: i8,
    pub corner_radius: u8,
}

/// Selected text, and the highlight of a selected toggle or combo entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection {
    pub fill: Color32,
    pub border: Stroke,
}

/// egui's own widgets (buttons, combo boxes, checkboxes, sliders) in each
/// interaction state.
#[derive(Debug, Clone, PartialEq)]
pub struct Widgets {
    /// Labels, separators and other things that can't be interacted with.
    pub noninteractive: Widget,
    pub inactive: Widget,
    pub hovered: Widget,
    /// Being pressed or dragged.
    pub active: Widget,
    /// A combo box or collapsing header that is open.
    pub open: Widget,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    /// Button and combo box background.
    pub button_fill: Color32,
    /// Checkbox, slider and other widget background.
    pub fill: Color32,
    pub border: Stroke,
    /// Strokes drawn on top: check marks, the close cross, arrows.
    pub foreground: Stroke,
    pub corner_radius: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Picker {
    pub title: Color32,
    pub title_selected_fill: Color32,
    pub row_selected_fill: Color32,
    pub info: Color32,
    pub cached: Color32,
    pub award_winner: Color32,
    pub award_nominee: Color32,
}

/// The texts drawn straight over the picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Hud {
    pub error: Color32,
    pub favorite: Color32,
    pub download: Color32,
    pub record: Color32,
    /// Drop shadow under every HUD text; its alpha follows the text's.
    pub shadow: Color32,
    /// Shadow offset as a fraction of the font size.
    pub shadow_offset: f32,
}

impl Theme {
    pub const DEFAULT: Theme = Theme {
        text: rgb(0xff, 0xff, 0xff),
        dim_text: rgb(0x80, 0x80, 0x80),
        panel: Panel {
            fill: Color32::from_black_alpha(230),
            border: stroke(2.0, rgb(0xff, 0xaa, 0x7c)),
            padding: 16,
            corner_radius: 0,
        },
        popup: Panel {
            fill: gray(27),
            border: stroke(1.0, gray(60)),
            padding: 6,
            corner_radius: 6,
        },
        field_fill: gray(10),
        selection: Selection {
            fill: rgb(0, 92, 128),
            border: stroke(1.0, rgb(192, 222, 255)),
        },
        widgets: Widgets {
            noninteractive: Widget {
                button_fill: gray(27),
                fill: gray(27),
                border: stroke(1.0, gray(60)),
                foreground: stroke(1.0, gray(140)),
                corner_radius: 2,
            },
            inactive: Widget {
                button_fill: gray(60),
                fill: gray(60),
                border: Stroke::NONE,
                foreground: stroke(1.0, gray(180)),
                corner_radius: 2,
            },
            hovered: Widget {
                button_fill: gray(70),
                fill: gray(70),
                border: stroke(1.0, gray(150)),
                foreground: stroke(1.5, gray(240)),
                corner_radius: 3,
            },
            active: Widget {
                button_fill: gray(55),
                fill: gray(55),
                border: stroke(1.0, Color32::WHITE),
                foreground: stroke(2.0, Color32::WHITE),
                corner_radius: 2,
            },
            open: Widget {
                button_fill: gray(45),
                fill: gray(27),
                border: stroke(1.0, gray(60)),
                foreground: stroke(1.0, gray(210)),
                corner_radius: 2,
            },
        },
        picker: Picker {
            title: rgb(0xff, 0xee, 0xcc),
            title_selected_fill: rgb(0xa0, 0x70, 0x50),
            row_selected_fill: rgb(0x00, 0x00, 0x6c),
            info: rgb(0xaa, 0xff, 0xe7),
            cached: Color32::WHITE,
            award_winner: rgb(0x80, 0x60, 0x10),
            award_nominee: rgb(0x60, 0x60, 0x80),
        },
        hud: Hud {
            error: rgb(0xa0, 0x10, 0x10),
            favorite: rgb(0xe0, 0x20, 0x20),
            download: rgb(0xe0, 0xff, 0xe0),
            record: rgb(0xff, 0x20, 0x20),
            shadow: Color32::BLACK,
            shadow_offset: 0.05,
        },
    };

    pub fn panel_frame(&self) -> egui::Frame {
        self.panel.frame()
    }

    fn apply(&self, style: &mut egui::Style) {
        style.spacing.menu_margin = egui::Margin::same(self.popup.padding);
        let visuals = &mut style.visuals;
        visuals.override_text_color = Some(self.text);
        visuals.window_fill = self.popup.fill;
        visuals.window_stroke = self.popup.border;
        visuals.window_corner_radius = egui::CornerRadius::same(self.popup.corner_radius);
        visuals.menu_corner_radius = egui::CornerRadius::same(self.popup.corner_radius);
        visuals.panel_fill = self.popup.fill;
        visuals.extreme_bg_color = self.field_fill;
        visuals.selection.bg_fill = self.selection.fill;
        visuals.selection.stroke = self.selection.border;
        let w = &self.widgets;
        let v = &mut visuals.widgets;
        for (from, to) in [
            (&w.noninteractive, &mut v.noninteractive),
            (&w.inactive, &mut v.inactive),
            (&w.hovered, &mut v.hovered),
            (&w.active, &mut v.active),
            (&w.open, &mut v.open),
        ] {
            to.weak_bg_fill = from.button_fill;
            to.bg_fill = from.fill;
            to.bg_stroke = from.border;
            to.fg_stroke = from.foreground;
            to.corner_radius = egui::CornerRadius::same(from.corner_radius);
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl Panel {
    pub fn frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.fill)
            .stroke(self.border)
            .inner_margin(egui::Margin::same(self.padding))
            .corner_radius(egui::CornerRadius::same(self.corner_radius))
    }
}

fn theme_id() -> egui::Id {
    egui::Id::new("retro_ui_theme")
}

/// Makes `theme` the one `ctx` is drawn with, egui's own widgets included.
pub fn set_theme(ctx: &egui::Context, theme: Theme) {
    ctx.all_styles_mut(|style| theme.apply(style));
    ctx.data_mut(|d| d.insert_temp(theme_id(), Arc::new(theme)));
}

/// The theme `ctx` is drawn with: [`Theme::DEFAULT`] until [`set_theme`].
pub fn theme(ctx: &egui::Context) -> Arc<Theme> {
    ctx.data(|d| d.get_temp(theme_id()))
        .unwrap_or_else(|| Arc::new(Theme::DEFAULT))
}

#[cfg(test)]
#[path = "tests/theme_tests.rs"]
mod tests;
