use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass,
    egui::{self, Align, Color32, TextFormat, Ui, scroll_area::ScrollAreaOutput},
};
use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

use crate::fuzzy_list::{DEFAULT_MAX_RESULTS, FuzzySource};

use resvg::tiny_skia;
use resvg::usvg::{self, Tree};

/// Key the app font is registered under in [`egui::FontDefinitions::font_data`].
const APP_FONT: &str = "app";

pub struct EguiUiPlugin;

/// Keeps `font.ttf` alive for [`setup_egui`]. Loading through the asset server
/// rather than reading [`crate::frontend::system_dir`] directly means egui picks
/// up the very same face -- and the same hot-reloaded bytes -- as the Bevy UI in
/// [`crate::hud`] and [`crate::text_input`].
#[derive(Resource)]
struct AppFont(Handle<Font>);

fn load_font(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(AppFont(asset_server.load("font.ttf")));
}

const HEADING_SIZE: f32 = 72.0;
/// Multipliers on [`HEADING_SIZE`], one per corner HUD text, so each can be
/// sized on its own without touching [`egui::TextStyle::Heading`] elsewhere.
const TOP_LEFT_SCALE: f32 = 1.0;
const TOP_RIGHT_SCALE: f32 = 2.5;
const BOTTOM_LEFT_SCALE: f32 = 1.0;
const INFO_TEXT_SCALE: f32 = 1.0;
const BODY_SIZE: f32 = 32.0;
const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const MARGIN: egui::Vec2 = egui::vec2(64.0, 32.0);

static ICON_SVG: &[u8] = include_bytes!("../data/coupdecoeur.svg");

/// Rasterize an SVG (from bytes) into an egui::ColorImage at the given
/// pixel size. `target_size` is in physical pixels.
fn rasterize_svg(svg_bytes: &[u8], target_size: [u32; 2]) -> anyhow::Result<egui::ColorImage> {
    let opt = usvg::Options::default();

    // If your SVG uses system fonts (text elements), you need a fontdb.
    // Skip this if your SVG is pure vector shapes.
    // let mut fontdb = usvg::fontdb::Database::new();
    // fontdb.load_system_fonts();

    let tree = Tree::from_data(svg_bytes, &opt)?;

    let [w, h] = target_size;
    let mut pixmap =
        tiny_skia::Pixmap::new(w, h).ok_or_else(|| anyhow::anyhow!("invalid pixmap dimensions"))?;

    // Scale the SVG's own viewBox size to fit target_size.
    let svg_size = tree.size();
    let transform =
        tiny_skia::Transform::from_scale(w as f32 / svg_size.width(), h as f32 / svg_size.height());

    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny_skia::Pixmap stores premultiplied RGBA — egui::ColorImage
    // wants straight (non-premultiplied) RGBA, so unpremultiply per pixel.
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for px in pixmap.pixels() {
        let a = px.alpha();
        if a == 0 {
            rgba.extend_from_slice(&[0, 0, 0, 0]);
        } else {
            let unpremul = |c: u8| ((c as u32 * 255) / a as u32) as u8;
            rgba.extend_from_slice(&[
                unpremul(px.red()),
                unpremul(px.green()),
                unpremul(px.blue()),
                a,
            ]);
        }
    }

    Ok(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        &rgba,
    ))
}

/// Call once (e.g. lazily on first frame, or in app init) and cache the handle.
fn load_icon_texture(ctx: &egui::Context) -> anyhow::Result<egui::TextureHandle> {
    let image = rasterize_svg(ICON_SVG, [64, 64])?;
    Ok(ctx.load_texture("icon-svg", image, egui::TextureOptions::LINEAR))
}

fn setup_egui(
    mut contexts: EguiContexts,
    app_font: Res<AppFont>,
    fonts: Res<Assets<Font>>,
    mut done: Local<bool>,
    mut state: ResMut<Images>,
) -> Result {
    if *done {
        return Ok(());
    }
    let Some(font) = fonts.get(&app_font.0) else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;

    let heart = load_icon_texture(ctx)?;
    state.heart = Some(heart);

    // egui owns its font bytes (it re-parses them for its own atlas), so this
    // copies out of the Bevy asset instead of sharing the `Blob`.
    let mut font_defs = egui::FontDefinitions::default();
    font_defs.font_data.insert(
        APP_FONT.to_owned(),
        Arc::new(egui::FontData::from_owned(font.data.data().to_vec())),
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

    *done = true;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
    Error,
}

/// Opens the searchable list over `source`. Picking a row emits a
/// [`FuzzyListSelect`] carrying `id` back, so several callers can tell their
/// pickers apart; re-opening the same `id` restores the search text and the
/// selected row.
#[derive(Message, Clone)]
pub struct ShowFuzzyList {
    pub id: usize,
    pub source: Arc<dyn FuzzySource>,
}

/// Emitted when the user picks a row (Enter, or Shift+Enter — see
/// [`FuzzyListSelect::alt`]) in the list opened by [`ShowFuzzyList`].
#[derive(Message, Debug, Clone)]
pub struct FuzzyListSelect {
    /// The list's `id`, so callers can tell their pickers apart.
    pub id: usize,
    /// Stable id of the chosen item, as reported by [`FuzzySource::search`].
    pub item: usize,
    #[allow(dead_code)]
    pub text: String,
    /// Set when the row was picked with Shift held (Shift+Enter), asking the
    /// caller for its alternative action on the item rather than the default.
    pub alt: bool,
}

#[derive(Default, Message, Clone)]
pub struct SetHudText {
    pub text: String,
    pub delay: Duration,
    pub duration: Duration,
    pub location: HudLocation,
}

#[derive(Default, Debug, Clone)]
pub struct HudText {
    pub text: String,
    pub duration: Range<f32>,
}

#[derive(Resource, Default)]
pub struct HudState {
    current_texts: HashMap<HudLocation, HudText>,
    show_list: bool,
    /// Caller-chosen id of the open list, echoed back in [`FuzzyListSelect`].
    list_id: usize,
    /// The search box text. Owned by the [`egui::TextEdit`] in [`render_list`],
    /// which edits it in place; [`sync_results`] filters on it.
    list_query: String,
    /// The query `list_items` was filtered with, so the source is only asked
    /// again when the text actually changed (the box is polled every frame).
    list_last_query: Option<String>,
    /// The ids of the results currently shown, in display order. An id is what
    /// a selection reports, so it survives re-filtering.
    list_items: Vec<usize>,
    /// Index into `list_items` of the highlighted row.
    list_selected: usize,
    /// The list's scroll offset in points, mirrored out of the [`egui::ScrollArea`]
    /// so [`render_list`] can steer it when the selection moves out of view
    /// while leaving the wheel free otherwise.
    list_scroll: f32,
    /// Set when the list is (re-)opened: forces one re-query of the source and
    /// one scroll back to the restored selection.
    list_reopened: bool,
    list_info: String,
    /// Item `list_info` describes, so the source is only asked when the
    /// highlighted item changes. `None` when nothing is highlighted.
    list_info_item: Option<usize>,
    list_source: Option<Arc<dyn FuzzySource>>,
}

#[derive(Resource, Default)]
pub struct Images {
    heart: Option<egui::TextureHandle>,
}

impl HudState {
    /// Whether the file picker is up. It owns the keyboard while it is, so the
    /// callers that feed keys to the emulated machine swallow them instead.
    pub fn list_open(&self) -> bool {
        self.show_list
    }
}

/// Look of the boxes the list is drawn in, matching the Bevy UI widget in
/// `crate::fuzzy_list`: near-opaque black behind a 2px orange border.
const PANEL_FILL: egui::Color32 = egui::Color32::from_black_alpha(230);
const PANEL_STROKE: egui::Color32 = egui::Color32::from_rgb(0xff, 0xaa, 0x7c);
const PANEL_BORDER: f32 = 2.0;
const PANEL_PADDING: i8 = 16;
/// Vertical gap between the list box and the info box below it.
const PANEL_GAP: f32 = 6.0;

const QUERY_SIZE: f32 = 32.0;
const ROW_SIZE: f32 = 28.0;
/// Fixed height of every row, so the box does not resize as the list is
/// filtered or emptied.
const ROW_HEIGHT: f32 = ROW_SIZE * 1.3;
/// Fraction of the screen height the list box is allowed to take up.
const LIST_HEIGHT_FRACTION: f32 = 0.6;

const INFO_SIZE: f32 = 22.0;
/// How many lines of info the field below the list reserves room for. Fixed, so
/// the centred layout doesn't jump as the selection moves between items whose
/// info differs in length.
const INFO_LINES: f32 = 5.0;
const INFO_COLOR: egui::Color32 = egui::Color32::from_rgb(0xaa, 0xff, 0xe7);
/// Highlight behind the selected row: white at 25%, premultiplied (the
/// `from_white_alpha` helper isn't const).
const SELECTED_ROW_COLOR: egui::Color32 = egui::Color32::from_rgb(0x0, 0x0, 0x6c);
const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xa0, 0x10, 0x10);
/// How long [`SELECTED_ROW_COLOR`] takes to fade away once the selection has
/// left a row, so a row the user passed over dims out instead of blinking off.
const FADE_SECS: f32 = 0.5;

fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL_FILL)
        .stroke(egui::Stroke::new(PANEL_BORDER, PANEL_STROKE))
        .inner_margin(egui::Margin::same(PANEL_PADDING))
}

/// How many rows the list shows at once: whole rows filling
/// [`LIST_HEIGHT_FRACTION`] of the screen. Also the PageUp/PageDown step, so
/// the key moves the selection exactly one screenful.
fn visible_rows(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() * LIST_HEIGHT_FRACTION / ROW_HEIGHT)
        .floor()
        .max(1.0)
}

/// Re-filters the list against the search box. The source is asked only when
/// the query changed since the last frame -- or when the list was just opened,
/// since the source itself may be a new one by then.
fn sync_results(state: &mut HudState, source: &Arc<dyn FuzzySource>) {
    let changed = state.list_last_query.as_deref() != Some(state.list_query.as_str());
    if !changed && !state.list_reopened {
        return;
    }
    state.list_items = source.search(&state.list_query, DEFAULT_MAX_RESULTS);
    state.list_last_query = Some(state.list_query.clone());
    if changed {
        // A new filter starts at the top; a re-open keeps where the user was.
        state.list_selected = 0;
        state.list_scroll = 0.0;
    }
}

/// Draws the scrollable, fixed-row-height list of `items`, highlighting row
/// `selected` and scrolling the least that keeps it in view. Each visible row is
/// handed to `render` along with the rect it was allocated, so the items can be
/// anything at all -- only their painting is the caller's business.
///
/// Sizes itself: as wide as `ui` leaves room for, and [`visible_rows`] rows tall.
fn scroll_area<T>(
    ui: &mut Ui,
    selected: usize,
    list_scroll: f32,
    items: &[T],
    render: impl Fn(&mut Ui, egui::Rect, &T),
) -> ScrollAreaOutput<()> {
    let view_height = visible_rows(ui.ctx()) * ROW_HEIGHT;
    let id = ui.id();
    panel_frame()
        .show(ui, |ui| {
            let mut scroll = egui::ScrollArea::vertical()
                .max_height(view_height)
                .auto_shrink([false, false]);

            let top = selected as f32 * ROW_HEIGHT;
            // Clamped to the end of the content as well, the same way egui
            // clamps whatever offset it is handed.
            let max_offset = (items.len() as f32 * ROW_HEIGHT - view_height).max(0.0);
            let offset = list_scroll
                .min(top)
                .max(top + ROW_HEIGHT - view_height)
                .clamp(0.0, max_offset);
            scroll = scroll.vertical_scroll_offset(offset);
            ui.set_width(ui.available_width());
            // Manual text rendering below, spacing > 0 will make it incorrect
            ui.spacing_mut().item_spacing.y = 0.0;

            scroll.show_rows(ui, ROW_HEIGHT, items.len(), |ui, rows| {
                for row in rows {
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_HEIGHT),
                        egui::Sense::hover(),
                    );
                    // Fades live in egui's animation map, keyed by absolute row
                    // rather than by position in the viewport, so they follow
                    // their row when the list scrolls -- and keep decaying on
                    // wall-clock time while the row is scrolled out of sight.
                    let row_id = id.with(row);
                    let level = if row == selected {
                        // Zero time snaps the stored value, so the highlight
                        // appears at once and the fade later starts from full.
                        ui.ctx().animate_value_with_time(row_id, 1.0, 0.0);
                        1.0
                    } else {
                        ui.ctx().animate_value_with_time(row_id, 0.0, FADE_SECS)
                    };
                    if level > 0.0 {
                        ui.painter().rect_filled(
                            rect,
                            egui::CornerRadius::ZERO,
                            SELECTED_ROW_COLOR.linear_multiply(level),
                        );
                    }

                    render(ui, rect, &items[row]);
                }
            })
        })
        .inner
}

/// Draws the file picker: a search box above a scrollable, filtered view of
/// [`HudState::list_source`], with a fixed-height info field
/// ([`FuzzySource::get_info`]) below it, centred on screen. The search box takes
/// keyboard focus for as long as the picker is up, with Up/Down/PageUp/PageDown
/// moving the highlighted row, Enter (or Shift+Enter, which sets
/// [`FuzzyListSelect::alt`]) emitting a [`FuzzyListSelect`] for it and Escape
/// closing the picker without one.
fn render_list(
    ctx: &egui::Context,
    state: &mut HudState,
    images: &Images,
    writer: &mut MessageWriter<FuzzyListSelect>,
    render: impl Fn(&mut Ui, egui::Rect, usize),
) {
    if !state.show_list {
        return;
    }
    let Some(source) = state.list_source.clone() else {
        state.show_list = false;
        return;
    };
    sync_results(state, &source);

    let screen = ctx.content_rect();
    // Same proportions as the Bevy widget: as wide as the screen is tall (it is
    // opened over a 4:3-ish emulator view), capped to what actually fits. The
    // panels below take their own width from this one, via `available_width`.
    let width = screen.height().min(screen.width() - 2.0 * MARGIN.x);

    // Selection keys are taken before the search box is drawn, so the
    // `TextEdit` never sees them. Home/End are deliberately left alone -- they
    // stay cursor movement inside the query. Counting (rather than testing)
    // the presses keeps a held-down arrow moving at the key repeat rate even
    // when several repeats land in one frame.
    let len = state.list_items.len();
    let (row_steps, page_steps, pick, alt, close) = ctx.input_mut(|i| {
        let none = egui::Modifiers::NONE;
        let rows = i.count_and_consume_key(none, egui::Key::ArrowDown) as i64
            - i.count_and_consume_key(none, egui::Key::ArrowUp) as i64;
        let pages = i.count_and_consume_key(none, egui::Key::PageDown) as i64
            - i.count_and_consume_key(none, egui::Key::PageUp) as i64;
        // Enter belongs to the list, not the search box, and Escape closes the
        // whole picker; both are consumed so the `TextEdit` never acts on them.
        // Shift+Enter picks the row as well, reported back as `alt` so a
        // caller can offer a second action on the same item. Taken before the
        // plain Enter, whose `NONE` modifiers would not match it anyway.
        let alt = i.consume_key(egui::Modifiers::SHIFT, egui::Key::Enter);
        let pick = alt || i.consume_key(none, egui::Key::Enter);
        let close = i.consume_key(none, egui::Key::Escape);
        (rows, pages, pick, alt, close)
    });
    if close {
        state.show_list = false;
        return;
    }
    let delta = row_steps + page_steps * visible_rows(ctx) as i64;
    let selected = if len == 0 {
        0
    } else {
        // Clamped rather than wrapped, and re-clamped against the current
        // length in case the list shrank since the last frame.
        (state.list_selected.min(len - 1) as i64 + delta).clamp(0, len as i64 - 1) as usize
    };
    state.list_selected = selected;

    if pick && let Some(&item) = state.list_items.get(selected) {
        writer.write(FuzzyListSelect {
            id: state.list_id,
            item,
            text: source.get_text(item),
            alt,
        });
        state.show_list = false;
        return;
    }

    // Describe the highlighted item, asking the source only when it changes
    // (arrow keys, or a new filter) rather than every frame.
    let info_item = state.list_items.get(selected).copied();
    if info_item != state.list_info_item {
        state.list_info = info_item.map(|id| source.get_info(id)).unwrap_or_default();
        state.list_info_item = info_item;
    }

    egui::Area::new(egui::Id::new("fuzzy_list"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = PANEL_GAP;

            panel_frame().show(ui, |ui| {
                let inner_width = ui.available_width();
                ui.set_width(inner_width);
                // The picker is modal, so the search box keeps focus the whole
                // time it is up: egui only routes key events to a focused
                // widget, and a click on the emulator behind would otherwise
                // take focus away and leave typing going nowhere.
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.list_query)
                        // Our own `panel_frame` already draws the box.
                        .frame(egui::Frame::NONE)
                        .margin(egui::Margin::ZERO)
                        .desired_width(inner_width)
                        .font(egui::FontId::proportional(QUERY_SIZE))
                        .text_color(TEXT_COLOR),
                );
                if !response.has_focus() {
                    response.request_focus();
                }
            });

            let scrolled = scroll_area(
                ui,
                selected,
                state.list_scroll,
                &state.list_items,
                |ui, rect, &id| render(ui, rect, id),
            );
            state.list_scroll = scrolled.state.offset.y;

            // The info box stays hidden while there is nothing to say.
            if !state.list_info.is_empty() {
                panel_frame().show(ui, |ui| {
                    ui.image((images.heart.as_ref().unwrap().id(), egui::vec2(32.0, 32.0)));
                    ui.set_width(ui.available_width());
                    ui.set_min_height(INFO_LINES * INFO_SIZE * 1.3);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&state.list_info)
                                .size(INFO_SIZE)
                                .color(INFO_COLOR),
                        )
                        .wrap(),
                    );
                });
            }
        });

    state.list_reopened = false;
}

/// Colour of the download counter, distinct from the HUD texts sharing the
/// corner so the two don't read as one line when both are up.
const DOWNLOAD_COLOR: egui::Color32 = egui::Color32::from_rgb(0xe0, 0xff, 0xe0);
/// Size of the download counter. Deliberately smaller than the corner HUD
/// texts: it is status, not a title.
const DOWNLOAD_SIZE: f32 = 32.0;

/// Draws how many downloads are in flight in the top-left corner, and nothing
/// at all while there are none. A placeholder for a real progress bar -- the
/// byte counts behind it are already tracked, see
/// [`Emulator::load_progress`](crate::emulator::Emulator::load_progress).
fn render_downloads(ctx: &egui::Context, pos: egui::Pos2) {
    let count = crate::emu_file::downloads_in_progress();
    let id = egui::Id::new("downloads");
    let t = ctx.animate_bool_with_time(id, count > 0, 1.0);
    if count == 0 || t < 0.5 {
        return;
    }
    egui::Area::new(id)
        .fixed_pos(pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            heading_with_shadow(
                ui,
                format!("\u{f409} {count}").as_str(),
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

fn update_ui(
    mut contexts: EguiContexts,
    mut state: ResMut<HudState>,
    images: Res<Images>,
    time: Res<Time>,
    mut selected: MessageWriter<FuzzyListSelect>,
    window: Single<&mut Window, With<PrimaryWindow>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;
    let scale = (window.height() / 1600.0).clamp(0.2, 8.0);
    ctx.set_pixels_per_point(window.scale_factor() * scale);

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
                let info = state.current_texts.get(&text).cloned().unwrap_or_default();
                let on = info.duration.contains(&time.elapsed_secs());
                let t = ctx.animate_bool_with_time(egui::Id::new(&info.text), on, 0.5);
                if t > 0.0 {
                    let quad = egui::Rect::from_two_pos(corner.pos_in_rect(&rect), rect.center());
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

                    ui.scope_builder(egui::UiBuilder::new().max_rect(quad).layout(layout), |ui| {
                        heading_with_shadow(
                            ui,
                            &info.text,
                            HEADING_SIZE * scale,
                            color,
                            corner.x(),
                        );
                    });
                }
            }
        });

    render_downloads(ctx, rect.min);
    // Cloned out before `state` is borrowed mutably below; the row painter
    // looks each visible row's text up through it. `render_list` bails out
    // itself when there is no source, so the painter never runs without one.
    let source = state.list_source.clone();
    render_list(ctx, &mut state, &images, &mut selected, |ui, rect, id| {
        let Some(source) = source.as_ref() else {
            return;
        };
        let text = source.get_text(id);
        let clip = egui::Rect::from_x_y_ranges(rect.x_range(), ui.clip_rect().y_range());
        let font = egui::FontId::proportional(ROW_SIZE);
        let format = TextFormat {
            font_id: font.clone(),
            extra_letter_spacing: -8.0, // negative = pull glyphs together
            color: Color32::from_rgb(220, 220, 20),
            valign: Align::Center,
            ..Default::default()
        };

        let mut job = egui::text::LayoutJob::default();
        job.append(
            &text,
            0.0,
            egui::TextFormat::simple(font.clone(), TEXT_COLOR),
        );
        job.append("  \u{f091} ", 0.0, format);

        let galley = ui.painter().layout_job(job);
        let pos = egui::Align2::LEFT_CENTER
            .anchor_size(rect.left_center(), galley.size())
            .min;
        let painter = ui.painter().with_clip_rect(clip);
        painter.galley(pos, galley.clone(), TEXT_COLOR);
        // Position the image right after the last glyph's end.
        let end_x = pos.x + galley.rect.width();
        let mut image_rect =
            egui::Rect::from_min_size(egui::pos2(end_x, pos.y), egui::vec2(32.0, 32.0));
        let tid = images.heart.as_ref().unwrap().id();
        for _ in 0..3 {
            egui::Image::new((tid, egui::vec2(16.0, 16.0))).paint_at(ui, image_rect);
            image_rect.min.x += 12.0;
            image_rect.max.x += 12.0;
        }
    });
    Ok(())
}

fn spawn_toast(
    mut state: ResMut<HudState>,
    time: Res<Time>,
    mut reader: MessageReader<SetHudText>,
) {
    for msg in reader.read() {
        info!("MSG: {}", msg.text);
        let now = time.elapsed_secs();
        let start = now + msg.delay.as_secs_f32();
        let stop = start + msg.duration.as_secs_f32();
        state.current_texts.insert(
            msg.location,
            HudText {
                text: msg.text.clone(),
                duration: (start..stop),
            },
        );
    }
}

fn open_fuzzy_list(mut state: ResMut<HudState>, mut reader: MessageReader<ShowFuzzyList>) {
    for msg in reader.read() {
        // A different picker starts fresh; the same one comes back with the
        // search text and the row the user left it on.
        if state.list_id != msg.id {
            state.list_id = msg.id;
            state.list_query.clear();
            state.list_selected = 0;
            state.list_scroll = 0.0;
        }
        state.list_source = Some(msg.source.clone());
        // The source may be a different instance than last time (rebuilt, or
        // just re-measured for the info field), so re-query and re-describe.
        state.list_reopened = true;
        state.list_info_item = None;
        state.show_list = true;
    }
}

impl Plugin for EguiUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default());
        // `EguiPlugin::build` has already run and inserted the resource, so this
        // lands before any camera is spawned in `Startup`.
        app.world_mut()
            .resource_mut::<EguiGlobalSettings>()
            .auto_create_primary_context = false;
        app.add_systems(Startup, load_font)
            .add_systems(EguiPrimaryContextPass, (setup_egui, update_ui).chain())
            .add_message::<SetHudText>()
            .add_message::<ShowFuzzyList>()
            .add_message::<FuzzyListSelect>()
            .add_systems(
                Update,
                (
                    spawn_toast.run_if(on_message::<SetHudText>),
                    open_fuzzy_list.run_if(on_message::<ShowFuzzyList>),
                ),
            )
            .insert_resource(Images::default())
            .insert_resource(HudState::default());
    }
}
