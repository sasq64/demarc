//! The corner texts and the download counter: the app's own overlay, drawn over
//! the picture.

use std::{collections::HashMap, ops::Range, time::Duration};

use egui::Ui;

use crate::{HEADING_SIZE, MARGIN};

/// Multipliers on [`HEADING_SIZE`], one per corner HUD text, so each can be
/// sized on its own without touching [`egui::TextStyle::Heading`] elsewhere.
const TOP_LEFT_SCALE: f32 = 1.0;
const TOP_RIGHT_SCALE: f32 = 2.5;
const BOTTOM_LEFT_SCALE: f32 = 1.0;
const INFO_TEXT_SCALE: f32 = 1.0;
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xa0, 0x10, 0x10);

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
    Error,
}

#[derive(Default, Debug, Clone)]
struct HudText {
    text: String,
    duration: Range<f32>,
}

#[derive(Default)]
pub struct Hud {
    current_texts: HashMap<HudLocation, HudText>,
}

impl Hud {
    /// Shows `text` at `location` from `now + delay` for `duration`. `now` is
    /// on the same clock as the one handed to [`Hud::show`].
    pub fn set_text(
        &mut self,
        location: HudLocation,
        text: &str,
        now: f32,
        delay: Duration,
        duration: Duration,
    ) {
        // An empty text retires whatever is showing in that corner. The entry
        // is kept with its duration ended rather than dropped, because the
        // fade-out in `show` animates on the text's own id — remove the
        // entry and the text blinks out instead of fading.
        if text.is_empty() {
            if let Some(hud) = self.current_texts.get_mut(&location) {
                hud.duration.end = now;
            }
            return;
        }
        let start = now + delay.as_secs_f32();
        let stop = start + duration.as_secs_f32();
        self.current_texts.insert(
            location,
            HudText {
                text: text.to_owned(),
                duration: (start..stop),
            },
        );
    }

    /// Draws the corner texts, and `download_bytes` left to download while
    /// there are any.
    pub fn show(&self, ctx: &egui::Context, now: f32, download_bytes: u64) {
        let rect = ctx.content_rect().shrink2(MARGIN);

        egui::Area::new(egui::Id::new("overlay"))
            .fixed_pos(rect.min)
            .order(egui::Order::Background)
            .show(ctx, |ui| {
                ui.set_min_size(rect.size());
                for (text, corner, scale) in [
                    (HudLocation::TopLeft, egui::Align2::LEFT_TOP, TOP_LEFT_SCALE),
                    (
                        HudLocation::TopRight,
                        egui::Align2::RIGHT_TOP,
                        TOP_RIGHT_SCALE,
                    ),
                    (
                        HudLocation::BottomLeft,
                        egui::Align2::LEFT_BOTTOM,
                        BOTTOM_LEFT_SCALE,
                    ),
                    (
                        HudLocation::InfoText,
                        egui::Align2::RIGHT_BOTTOM,
                        INFO_TEXT_SCALE,
                    ),
                    (
                        HudLocation::Error,
                        egui::Align2::LEFT_BOTTOM,
                        BOTTOM_LEFT_SCALE * 0.75,
                    ),
                ] {
                    let info = self.current_texts.get(&text).cloned().unwrap_or_default();
                    let on = info.duration.contains(&now);
                    let t = ctx.animate_bool_with_time(egui::Id::new(&info.text), on, 0.5);
                    if t > 0.0 {
                        let quad =
                            egui::Rect::from_two_pos(corner.pos_in_rect(&rect), rect.center());
                        let layout = if corner.y() == egui::Align::Min {
                            egui::Layout::top_down(corner.x())
                        } else {
                            egui::Layout::bottom_up(corner.x())
                        };

                        let color = (if text == HudLocation::Error {
                            ERROR_COLOR
                        } else {
                            ui.visuals().text_color()
                        })
                        .linear_multiply(t);

                        ui.scope_builder(
                            egui::UiBuilder::new().max_rect(quad).layout(layout),
                            |ui| {
                                heading_with_shadow(
                                    ui,
                                    &info.text,
                                    HEADING_SIZE * scale,
                                    color,
                                    corner.x(),
                                );
                            },
                        );
                    }
                }
            });

        render_downloads(ctx, rect.min, download_bytes);
    }
}

/// Colour of the download counter, distinct from the HUD texts sharing the
/// corner so the two don't read as one line when both are up.
const DOWNLOAD_COLOR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0xff, 0xe0);
/// Size of the download counter. Deliberately smaller than the corner HUD
/// texts: it is status, not a title.
const DOWNLOAD_SIZE: f32 = 32.0;

/// Draws how many bytes are left to download in the top-left corner, and
/// nothing at all while there are none.
fn render_downloads(ctx: &egui::Context, pos: egui::Pos2, bytes: u64) {
    let id = egui::Id::new("downloads");
    let t = ctx.animate_bool_with_time(id, bytes > 0, 1.0);
    if bytes == 0 || t < 0.5 {
        return;
    }
    let text = if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    };
    egui::Area::new(id)
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            heading_with_shadow(
                ui,
                format!("\u{f409} {text}").as_str(),
                DOWNLOAD_SIZE,
                DOWNLOAD_COLOR.linear_multiply((t - 0.5) * 2.0),
                egui::Align::Min,
            );
        });
}

/// How far the drop shadow under the corner HUD texts is offset, as a fraction
/// of the font size, so it keeps its look at every [`HEADING_SIZE`] scale.
const SHADOW_OFFSET: f32 = 0.05;

/// Draws `text` as a heading with a solid black copy of itself painted first,
/// offset down and to the right, so it stays readable over a bright emulator
/// picture. Laid out by hand rather than as two [`Ui::heading`] calls, which
/// would stack the shadow below the text instead of behind it.
///
/// `align` is the edge the individual lines line up on, so a multi-line text in
/// a right-hand corner reads flush against that corner rather than ragged.
fn heading_with_shadow(
    ui: &mut Ui,
    text: &str,
    size: f32,
    color: egui::Color32,
    align: egui::Align,
) {
    // Laid out uncoloured so the one galley can be painted twice, with
    // `Painter::galley` filling in a colour each time. A job rather than
    // `layout_no_wrap`, which has no say over `halign`.
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::proportional(size), egui::Color32::PLACEHOLDER),
    );
    job.halign = align;
    let galley = ui.painter().layout_job(job);
    let offset = egui::Vec2::splat(size * SHADOW_OFFSET);
    // The shadow is part of the text as far as the layout is concerned, so the
    // corner it is anchored in leaves room for it.
    let (rect, _) = ui.allocate_exact_size(galley.size() + offset, egui::Sense::hover());
    // `halign` puts the galley's origin on that same edge (x = 0 is the right
    // edge for `Align::Max`), so the box just allocated is mapped onto it by
    // cancelling out where the galley starts.
    let pos = rect.min - galley.rect.min.to_vec2();
    // Black at the text's own alpha, so shadow and text fade together.
    let shadow = egui::Color32::from_black_alpha(color.a());
    ui.painter().galley(pos + offset, galley.clone(), shadow);
    ui.painter().galley(pos, galley, color);
}

#[cfg(test)]
#[path = "tests/hud_tests.rs"]
mod tests;
