//! Every colour, stroke and fill the UI is drawn with, in one place. Layout and
//! sizes stay with the code that lays things out.

use std::sync::Arc;

use egui::{Color32, Stroke};
use serde::{Deserialize, Serialize};

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn gray(l: u8) -> Color32 {
    Color32::from_gray(l)
}

const fn stroke(width: f32, color: Color32) -> Stroke {
    Stroke { width, color }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub text: Text,
    pub panel: Panel,
    /// Combo box drop-downs and other popups.
    pub popup: Panel,
    /// Text fields and drag values.
    pub field: Field,
    pub selection: Selection,
    pub widgets: Widgets,
    pub picker: Picker,
    pub hud: Hud,
    /// The whole screen while nothing is loaded.
    #[serde(default)]
    pub screen: Screen,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Screen {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader: Option<ShaderBg>,
}

/// A WGSL fragment shader drawn as a background, with an optional PNG it can
/// sample. Paths are relative to the theme file.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShaderBg {
    pub shader: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub texture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Text {
    /// TTF/OTF for the whole UI, relative to the theme file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(with = "hex")]
    pub color: Color32,
    /// Hints and disabled entries.
    #[serde(with = "hex")]
    pub dim: Color32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(with = "hex")]
    pub fill: Color32,
}

/// The box the picker and the dialogs are drawn in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    #[serde(with = "hex")]
    pub fill: Color32,
    #[serde(with = "StrokeDef")]
    pub border: Stroke,
    pub padding: i8,
    pub corner_radius: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glow: Option<Glow>,
    /// Drawn under the fill, so a translucent fill tints it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shader: Option<ShaderBg>,
}

/// A soft halo around a panel, drawn outside its border.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Glow {
    /// Width of the fuzzy edge; 0 is a sharp one.
    pub blur: u8,
    /// How far the halo extends past the panel before it starts to fade.
    pub spread: u8,
    #[serde(with = "hex")]
    pub color: Color32,
}

impl Glow {
    fn shadow(&self) -> egui::Shadow {
        egui::Shadow {
            offset: [0, 0],
            blur: self.blur,
            spread: self.spread,
            color: self.color,
        }
    }
}

/// Selected text, and the highlight of a selected toggle or combo entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    #[serde(with = "hex")]
    pub fill: Color32,
    #[serde(with = "StrokeDef")]
    pub border: Stroke,
}

/// egui's own widgets (buttons, combo boxes, checkboxes, sliders) in each
/// interaction state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    /// Button and combo box background.
    #[serde(with = "hex")]
    pub button_fill: Color32,
    /// Checkbox, slider and other widget background.
    #[serde(with = "hex")]
    pub fill: Color32,
    #[serde(with = "StrokeDef")]
    pub border: Stroke,
    /// Strokes drawn on top: check marks, the close cross, arrows.
    #[serde(with = "StrokeDef")]
    pub foreground: Stroke,
    pub corner_radius: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Picker {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(with = "hex")]
    pub text: Color32,
    #[serde(with = "hex")]
    pub title: Color32,
    #[serde(with = "hex")]
    pub title_selected_fill: Color32,
    #[serde(with = "hex")]
    pub row_selected_fill: Color32,
    #[serde(with = "hex")]
    pub info: Color32,
    #[serde(with = "hex")]
    pub cached: Color32,
    #[serde(with = "hex")]
    pub award_winner: Color32,
    #[serde(with = "hex")]
    pub award_nominee: Color32,
    /// Mixed into list icons by its alpha: opaque replaces their colour.
    #[serde(with = "hex")]
    pub icon_color: Color32,
}

/// The texts drawn straight over the picture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hud {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(with = "hex")]
    pub text: Color32,
    #[serde(with = "hex")]
    pub error: Color32,
    #[serde(with = "hex")]
    pub favorite: Color32,
    #[serde(with = "hex")]
    pub download: Color32,
    #[serde(with = "hex")]
    pub record: Color32,
    /// Drop shadow under every HUD text; its alpha follows the text's.
    #[serde(with = "hex")]
    pub shadow: Color32,
    /// Shadow offset as a fraction of the font size.
    pub shadow_offset: f32,
}

impl Picker {
    /// `color` with its RGB moved towards `icon_color` by that one's alpha.
    pub fn tint_icon(&self, color: Color32) -> Color32 {
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        let [tr, tg, tb, ta] = self.icon_color.to_srgba_unmultiplied();
        let mix = |c: u8, t: u8| ((c as u32 * (255 - ta as u32) + t as u32 * ta as u32 + 127) / 255) as u8;
        Color32::from_rgba_unmultiplied(mix(r, tr), mix(g, tg), mix(b, tb), a)
    }
}

impl Theme {
    pub const DEFAULT: Theme = Theme {
        text: Text {
            font: None,
            color: rgb(0xff, 0xff, 0xff),
            dim: rgb(0x80, 0x80, 0x80),
        },
        panel: Panel {
            fill: Color32::from_black_alpha(230),
            border: stroke(2.0, rgb(0xff, 0xaa, 0x7c)),
            padding: 16,
            corner_radius: 0,
            glow: None,
            shader: None,
        },
        popup: Panel {
            fill: gray(27),
            border: stroke(1.0, gray(60)),
            padding: 6,
            corner_radius: 6,
            glow: None,
            shader: None,
        },
        field: Field { fill: gray(10) },
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
            font: None,
            text: Color32::WHITE,
            title: rgb(0xff, 0xee, 0xcc),
            title_selected_fill: rgb(0xa0, 0x70, 0x50),
            row_selected_fill: rgb(0x00, 0x00, 0x6c),
            info: rgb(0xaa, 0xff, 0xe7),
            cached: Color32::WHITE,
            award_winner: rgb(0x80, 0x60, 0x10),
            award_nominee: rgb(0x60, 0x60, 0x80),
            icon_color: Color32::TRANSPARENT,
        },
        hud: Hud {
            font: None,
            text: Color32::WHITE,
            error: rgb(0xa0, 0x10, 0x10),
            favorite: rgb(0xe0, 0x20, 0x20),
            download: rgb(0xe0, 0xff, 0xe0),
            record: rgb(0xff, 0x20, 0x20),
            shadow: Color32::BLACK,
            shadow_offset: 0.05,
        },
        screen: Screen { shader: None },
    };

    /// Reads a theme file: `[palette]` names colours that any colour value
    /// may then use instead of a `#rrggbb[aa]` hex code, and whatever the file
    /// leaves out is taken from [`Theme::DEFAULT`].
    /// The fonts the theme names, for the UI, the picker and the HUD.
    pub fn fonts(&self) -> [Option<&str>; 3] {
        [&self.text.font, &self.picker.font, &self.hud.font].map(Option::as_deref)
    }

    pub fn from_toml(src: &str) -> Result<Theme, String> {
        let mut file: toml::Table = src.parse().map_err(|e| format!("{e}"))?;
        if let Some(palette) = file.remove("palette") {
            let toml::Value::Table(palette) = palette else {
                return Err("`palette` must be a table".into());
            };
            for (_, value) in file.iter_mut() {
                use_palette(value, &palette)?;
            }
        }
        let mut theme = toml::Table::try_from(Theme::DEFAULT).map_err(|e| e.to_string())?;
        merge(&mut theme, file);
        theme.try_into().map_err(|e: toml::de::Error| e.to_string())
    }

    fn apply(&self, style: &mut egui::Style) {
        style.spacing.menu_margin = egui::Margin::same(self.popup.padding);
        let visuals = &mut style.visuals;
        visuals.override_text_color = Some(self.text.color);
        visuals.window_fill = self.popup.fill;
        visuals.window_stroke = self.popup.border;
        if let Some(glow) = &self.popup.glow {
            visuals.window_shadow = glow.shadow();
            visuals.popup_shadow = glow.shadow();
        }
        visuals.window_corner_radius = egui::CornerRadius::same(self.popup.corner_radius);
        visuals.menu_corner_radius = egui::CornerRadius::same(self.popup.corner_radius);
        visuals.panel_fill = self.popup.fill;
        visuals.extreme_bg_color = self.field.fill;
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

fn use_palette(value: &mut toml::Value, palette: &toml::Table) -> Result<(), String> {
    match value {
        toml::Value::String(name) if !name.starts_with('#') => match palette.get(name.as_str()) {
            Some(toml::Value::String(color)) => *name = color.clone(),
            _ => return Err(format!("`{name}` is not in the palette")),
        },
        toml::Value::Table(table) => {
            for (_, value) in table.iter_mut().filter(|(key, _)| !matches!(key.as_str(), "shader" | "font")) {
                use_palette(value, palette)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(over)) => merge(base, over),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Colours are written `#rrggbb`, or `#rrggbbaa` with unmultiplied alpha.
mod hex {
    use egui::Color32;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(color: &Color32, s: S) -> Result<S::Ok, S::Error> {
        let [r, g, b, a] = color.to_srgba_unmultiplied();
        let hex = if a == 255 {
            format!("#{r:02x}{g:02x}{b:02x}")
        } else {
            format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
        };
        s.serialize_str(&hex)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color32, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text)
            .ok_or_else(|| D::Error::custom(format!("`{text}` is not a #rrggbb[aa] colour")))
    }

    fn parse(text: &str) -> Option<Color32> {
        let hex = text.strip_prefix('#')?;
        if !matches!(hex.len(), 6 | 8) || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let a = if hex.len() == 8 { byte(6)? } else { 255 };
        Some(Color32::from_rgba_unmultiplied(
            byte(0)?,
            byte(2)?,
            byte(4)?,
            a,
        ))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Stroke", deny_unknown_fields)]
struct StrokeDef {
    width: f32,
    #[serde(with = "hex")]
    color: Color32,
}

impl Panel {
    pub fn frame(&self) -> egui::Frame {
        egui::Frame::new()
            .fill(self.fill)
            .stroke(self.border)
            .inner_margin(egui::Margin::same(self.padding))
            .corner_radius(egui::CornerRadius::same(self.corner_radius))
            .shadow(self.glow.as_ref().map_or(egui::Shadow::NONE, Glow::shadow))
    }

    /// Draws `add_contents` in this panel, over its shader if it has one.
    pub fn show<R>(
        &self,
        ui: &mut egui::Ui,
        add_contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        let slot = ui.painter().add(egui::Shape::Noop);
        let frame = self.frame();
        let inner = frame.shadow(egui::Shadow::NONE).show(ui, add_contents);
        if frame.shadow != egui::Shadow::NONE {
            // Clipped to the four strips outside the panel, so none of it
            // shows through a translucent fill.
            let rect = inner.response.rect;
            let glow = frame.shadow.as_shape(rect, frame.corner_radius);
            let outer = glow.visual_bounding_rect();
            for clip in [
                egui::Rect::from_x_y_ranges(outer.x_range(), outer.top()..=rect.top()),
                egui::Rect::from_x_y_ranges(outer.x_range(), rect.bottom()..=outer.bottom()),
                egui::Rect::from_x_y_ranges(outer.left()..=rect.left(), rect.y_range()),
                egui::Rect::from_x_y_ranges(rect.right()..=outer.right(), rect.y_range()),
            ] {
                ui.painter().with_clip_rect(clip).add(glow.clone());
            }
        }
        if let Some(shape) = self
            .shader
            .as_ref()
            .and_then(|bg| shader_shape(ui.ctx(), inner.response.rect, bg))
        {
            ui.painter().set(slot, shape);
        }
        inner
    }
}

/// Turns a [`ShaderBg`] over a rect into whatever the renderer draws it with.
pub type ShaderPainter = Arc<dyn Fn(egui::Rect, &ShaderBg) -> Option<egui::Shape> + Send + Sync>;

fn shader_painter_id() -> egui::Id {
    egui::Id::new("retro_ui_shader_painter")
}

/// Without one, theme shaders are not drawn.
pub fn set_shader_painter(ctx: &egui::Context, painter: ShaderPainter) {
    ctx.data_mut(|d| d.insert_temp(shader_painter_id(), painter));
}

fn shader_shape(ctx: &egui::Context, rect: egui::Rect, bg: &ShaderBg) -> Option<egui::Shape> {
    let painter = ctx.data(|d| d.get_temp::<ShaderPainter>(shader_painter_id()))?;
    ctx.request_repaint();
    painter(rect, bg)
}

/// Fills the screen, behind everything else, with the theme's screen shader.
pub fn paint_screen(ctx: &egui::Context) {
    let theme = theme(ctx);
    let Some(bg) = &theme.screen.shader else {
        return;
    };
    if let Some(shape) = shader_shape(ctx, ctx.content_rect(), bg) {
        ctx.layer_painter(egui::LayerId::background()).add(shape);
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
