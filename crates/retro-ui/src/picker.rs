//! The searchable list: a search box above a scrollable, filtered view of a
//! [`FuzzySource`], with an info field below it.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

use egui::{Ui, scroll_area::ScrollAreaOutput};

use crate::fuzzy_list::{DEFAULT_MAX_RESULTS, FuzzySource, ListIcon};
use crate::{MARGIN, TEXT_COLOR, panel_frame, take_key};

pub type ListSource<T> = Arc<dyn FuzzySource<T>>;

/// Vertical gap between the list box and the info box below it.
const PANEL_GAP: f32 = 6.0;

const QUERY_SIZE: f32 = 32.0;
pub const ROW_SIZE: f32 = 28.0;
/// Fixed height of every row, so the box does not resize as the list is
/// filtered or emptied.
const ROW_HEIGHT: f32 = ROW_SIZE * 1.3;
/// Fraction of the screen height the list box is allowed to take up.
const LIST_HEIGHT_FRACTION: f32 = 0.6;
/// Side of the square icon column at the left of every row, and the gap between
/// it and the row's text. Fixed, so rows line up whether they have an icon or not.
const ICON_SIZE: f32 = ROW_SIZE;
const ICON_GAP: f32 = 8.0;

const TITLE_SIZE: f32 = 26.0;
const TITLE_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xaa, 0x7c);

const INFO_SIZE: f32 = 22.0;
/// How many lines of info the field below the list reserves room for. Fixed, so
/// the centred layout doesn't jump as the selection moves between items whose
/// info differs in length.
const INFO_LINES: f32 = 5.0;
const INFO_COLOR: egui::Color32 = egui::Color32::from_rgb(0xaa, 0xff, 0xe7);
const SELECTED_ROW_COLOR: egui::Color32 = egui::Color32::from_rgb(0x0, 0x0, 0x6c);
/// How long [`SELECTED_ROW_COLOR`] takes to fade away once the selection has
/// left a row, so a row the user passed over dims out instead of blinking off.
const FADE_SECS: f32 = 0.5;

/// Images available to [`ListIcon::Image`], by the id it names them with.
static LIST_ICONS: LazyLock<Mutex<HashMap<u32, Arc<egui::ColorImage>>>> =
    LazyLock::new(Default::default);

/// Registers `image` as the icon for `id`. Rows whose source returns
/// [`ListIcon::Image`] with that id draw it in their icon column.
pub fn add_list_icon(id: u32, image: egui::ColorImage) {
    if let Ok(mut icons) = LIST_ICONS.lock() {
        icons.insert(id, Arc::new(image));
    }
}

/// The texture for list icon `id` in `ctx`, uploaded on first use and cached
/// there -- one window's textures are no good to another's context.
fn list_icon_texture(ctx: &egui::Context, id: u32) -> Option<egui::TextureId> {
    let key = egui::Id::new(("list_icon", id));
    if let Some(handle) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(key)) {
        return Some(handle.id());
    }
    let image = LIST_ICONS.lock().ok()?.get(&id).cloned()?;
    let handle = ctx.load_texture(
        format!("list_icon_{id}"),
        image,
        egui::TextureOptions::LINEAR,
    );
    let texture_id = handle.id();
    ctx.data_mut(|d| d.insert_temp(key, handle));
    Some(texture_id)
}

/// Paints `icon` centred in `rect`.
fn draw_list_icon(ui: &Ui, painter: &egui::Painter, rect: egui::Rect, icon: ListIcon) {
    match icon {
        ListIcon::Glyph(ch, rgb) => {
            let [_, r, g, b] = rgb.to_be_bytes();
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                ch,
                egui::FontId::proportional(ICON_SIZE),
                egui::Color32::from_rgb(r, g, b),
            );
        }
        ListIcon::Image(id) => {
            if let Some(texture) = list_icon_texture(ui.ctx(), id) {
                egui::Image::new((texture, rect.size())).paint_at(ui, rect);
            }
        }
    }
}

/// Paints one row of the list: `icons` at its left and far right, and `job` as
/// its text. Returns where the text ended up, for a caller with more to draw
/// after it.
pub fn draw_row(
    ui: &Ui,
    rect: egui::Rect,
    job: egui::text::LayoutJob,
    icons: (Option<ListIcon>, Option<ListIcon>),
) -> egui::Rect {
    let clip = egui::Rect::from_x_y_ranges(rect.x_range(), ui.clip_rect().y_range());
    let galley = ui.painter().layout_job(job);
    let text_left = rect.left_center() + egui::vec2(ICON_SIZE + ICON_GAP, 0.0);
    let text_rect = egui::Align2::LEFT_CENTER.anchor_size(text_left, galley.size());
    let painter = ui.painter().with_clip_rect(clip);
    for (icon, x) in [
        (icons.0, rect.left() + ICON_SIZE / 2.0),
        (icons.1, rect.right() - ICON_SIZE / 2.0),
    ] {
        if let Some(icon) = icon {
            let icon_rect = egui::Rect::from_center_size(
                egui::pos2(x, rect.center().y),
                egui::Vec2::splat(ICON_SIZE),
            );
            draw_list_icon(ui, &painter, icon_rect, icon);
        }
    }
    painter.galley(text_rect.min, galley, TEXT_COLOR);
    text_rect
}

/// The row the user picked (Enter, or Shift+Enter -- see [`Picked::alt`]).
#[derive(Debug, Clone)]
pub struct Picked<T> {
    /// The list's `id`, so callers can tell their pickers apart.
    pub id: usize,
    /// Stable id of the chosen item, as reported by [`FuzzySource::search`].
    pub item: usize,
    pub text: String,
    /// Set when the row was picked with Shift held, asking the caller for its
    /// alternative action on the item rather than the default.
    pub alt: bool,
    pub data: Option<T>,
}

pub struct Picker<T> {
    show_list: bool,
    /// Caller-chosen id of the open list, echoed back in [`Picked`].
    list_id: usize,
    /// The search box text. Owned by the [`egui::TextEdit`] in [`Picker::show`],
    /// which edits it in place; [`Picker::sync_results`] filters on it.
    list_query: String,
    /// The query `list_items` was filtered with, so the source is only asked
    /// again when the text actually changed (the box is polled every frame).
    list_last_query: Option<String>,
    /// The ids of the results currently shown, in display order. An id is what
    /// a selection reports, so it survives re-filtering.
    list_items: Vec<usize>,
    /// Index into `list_items` of the highlighted row.
    list_selected: usize,
    /// Source id to highlight once the reopened list has been re-queried.
    list_pending_select: Option<usize>,
    /// The list's scroll offset in points, mirrored out of the [`egui::ScrollArea`]
    /// so [`Picker::show`] can steer it when the selection moves out of view
    /// while leaving the wheel free otherwise.
    list_scroll: f32,
    /// Set when the list is (re-)opened: forces one re-query of the source and
    /// one scroll back to the restored selection.
    list_reopened: bool,
    /// Text of the line above the search box.
    list_title: String,
    list_info: String,
    /// Item `list_info` describes, so the source is only asked when the
    /// highlighted item changes. `None` when nothing is highlighted.
    list_info_item: Option<usize>,
    list_source: Option<ListSource<T>>,
}

impl<T> Default for Picker<T> {
    fn default() -> Self {
        Self {
            show_list: false,
            list_id: 0,
            list_query: String::new(),
            list_last_query: None,
            list_items: Vec::new(),
            list_selected: 0,
            list_pending_select: None,
            list_scroll: 0.0,
            list_reopened: false,
            list_title: String::new(),
            list_info: String::new(),
            list_info_item: None,
            list_source: None,
        }
    }
}

impl<T: 'static> Picker<T> {
    pub fn is_open(&self) -> bool {
        self.show_list
    }

    /// The search box text, when the list currently open is `id`'s.
    pub fn query(&self, id: usize) -> Option<&str> {
        (self.show_list && self.list_id == id).then_some(self.list_query.as_str())
    }

    /// Source id of the highlighted row in list `id`, while it is open.
    pub fn selected_item(&self, id: usize) -> Option<usize> {
        if !self.show_list || self.list_id != id {
            return None;
        }
        self.list_items.get(self.list_selected).copied()
    }

    /// Opens the list over `source`. Re-opening the same `id` restores the
    /// search text and the selected row; `prompt` replaces the search text and
    /// `selected` is the source id of the row to highlight, if it is among the
    /// results. `title` is shown above the search box, and hidden while empty.
    pub fn open(
        &mut self,
        id: usize,
        source: ListSource<T>,
        prompt: Option<&str>,
        selected: Option<usize>,
        title: &str,
    ) {
        // A different picker starts fresh; the same one comes back with the
        // search text and the row the user left it on.
        if self.list_id != id {
            self.list_id = id;
            self.list_query.clear();
            self.list_selected = 0;
            self.list_scroll = 0.0;
        }
        if let Some(prompt) = prompt {
            self.list_query = prompt.into();
        }
        self.list_pending_select = selected;
        self.list_title = title.to_owned();
        self.list_source = Some(source);
        // The source may be a different instance than last time (rebuilt, or
        // just re-measured for the info field), so re-query and re-describe.
        self.list_reopened = true;
        self.list_info_item = None;
        self.show_list = true;
    }

    /// Re-filters the list against the search box. The source is asked only when
    /// the query changed since the last frame -- or when the list was just opened,
    /// since the source itself may be a new one by then.
    fn sync_results(&mut self, source: &ListSource<T>) {
        let changed = self.list_last_query.as_deref() != Some(self.list_query.as_str());
        if !changed && !self.list_reopened {
            return;
        }
        self.list_items = source.search(&self.list_query, DEFAULT_MAX_RESULTS);
        self.list_last_query = Some(self.list_query.clone());
        if changed {
            // A new filter starts at the top; a re-open keeps where the user was.
            self.list_selected = 0;
            self.list_scroll = 0.0;
        }
        if let Some(item) = self.list_pending_select.take()
            && let Some(pos) = self.list_items.iter().position(|&i| i == item)
        {
            self.list_selected = pos;
        }
    }

    /// Draws the picker, centred on screen, while it is open. The search box
    /// takes keyboard focus for as long as it is up, with
    /// Up/Down/PageUp/PageDown moving the highlighted row, Enter (or
    /// Shift+Enter, which sets [`Picked::alt`]) returning a [`Picked`] for it
    /// and Escape closing the picker without one.
    ///
    /// Each visible row is handed to `render` with the rect it was allocated,
    /// the source and the item's id; [`draw_row`] paints the usual one.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        render: impl Fn(&mut Ui, egui::Rect, &dyn FuzzySource<T>, usize),
    ) -> Option<Picked<T>> {
        if !self.show_list {
            return None;
        }
        let Some(source) = self.list_source.clone() else {
            self.show_list = false;
            return None;
        };
        self.sync_results(&source);

        let screen = ctx.content_rect();
        // As wide as the screen is tall (it is opened over a 4:3-ish emulator
        // view), capped to what actually fits. The panels below take their own
        // width from this one, via `available_width`.
        let width = screen.height().min(screen.width() - 2.0 * MARGIN.x);

        // Selection keys are taken before the search box is drawn, so the
        // `TextEdit` never sees them. Home/End are deliberately left alone -- they
        // stay cursor movement inside the query. Counting (rather than testing)
        // the presses keeps a held-down arrow moving at the key repeat rate even
        // when several repeats land in one frame.
        let len = self.list_items.len();
        let (row_steps, page_steps, pick, close, shift) = ctx.input_mut(|i| {
            let rows = take_key(i, egui::Key::ArrowDown) - take_key(i, egui::Key::ArrowUp);
            let pages = take_key(i, egui::Key::PageDown) - take_key(i, egui::Key::PageUp);
            // Enter belongs to the list, not the search box, and Escape closes the
            // whole picker; both are consumed so the `TextEdit` never acts on them,
            // and both are taken whatever is held alongside them.
            let pick = take_key(i, egui::Key::Enter) > 0;
            let close = take_key(i, egui::Key::Escape) > 0;
            (rows, pages, pick, close, i.modifiers.shift)
        });
        let alt = pick && shift;
        if close {
            self.show_list = false;
            return None;
        }
        let delta = row_steps + page_steps * visible_rows(ctx) as i64;
        let selected = if len == 0 {
            0
        } else {
            // Clamped rather than wrapped, and re-clamped against the current
            // length in case the list shrank since the last frame.
            (self.list_selected.min(len - 1) as i64 + delta).clamp(0, len as i64 - 1) as usize
        };
        self.list_selected = selected;

        if pick && let Some(&item) = self.list_items.get(selected) {
            self.show_list = false;
            return Some(Picked {
                id: self.list_id,
                item: source.get_item(item),
                text: source.get_text(item),
                alt,
                data: source.get_data(item),
            });
        }

        // Describe the highlighted item, asking the source only when it changes
        // (arrow keys, or a new filter) rather than every frame.
        let info_item = self.list_items.get(selected).copied();
        if info_item != self.list_info_item {
            self.list_info = info_item.map(|id| source.get_info(id)).unwrap_or_default();
            self.list_info_item = info_item;
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
                    if !self.list_title.is_empty() {
                        ui.add(egui::Label::new(
                            egui::RichText::new(&self.list_title)
                                .size(TITLE_SIZE)
                                .color(TITLE_COLOR),
                        ));
                    }
                    // The picker is modal, so the search box keeps focus the whole
                    // time it is up: egui only routes key events to a focused
                    // widget, and a click on the emulator behind would otherwise
                    // take focus away and leave typing going nowhere.
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.list_query)
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
                    self.list_scroll,
                    &self.list_items,
                    |ui, rect, &id| render(ui, rect, source.as_ref(), id),
                );
                self.list_scroll = scrolled.state.offset.y;

                // The info box stays hidden while there is nothing to say.
                if !self.list_info.is_empty() {
                    panel_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.set_min_height(INFO_LINES * INFO_SIZE * 1.3);
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&self.list_info)
                                    .size(INFO_SIZE)
                                    .color(INFO_COLOR),
                            )
                            .wrap(),
                        );
                    });
                }
            });

        self.list_reopened = false;
        None
    }
}

/// How many rows the list shows at once: whole rows filling
/// [`LIST_HEIGHT_FRACTION`] of the screen. Also the PageUp/PageDown step, so
/// the key moves the selection exactly one screenful.
fn visible_rows(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() * LIST_HEIGHT_FRACTION / ROW_HEIGHT)
        .floor()
        .max(1.0)
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
            // An anchored `Area` hands its content last frame's size as the
            // space available, so a scroll area that only caps its height
            // stays stuck at whatever the area started out as. Pinning the
            // minimum too makes the box exactly `view_height` from the first
            // frame on, however little the rest of the picker asks for.
            let mut scroll = egui::ScrollArea::vertical()
                .max_height(view_height)
                .min_scrolled_height(view_height)
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

#[cfg(test)]
#[path = "tests/picker_tests.rs"]
mod tests;
