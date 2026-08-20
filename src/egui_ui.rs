use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, egui};
use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

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

const HEADING_SIZE: f32 = 60.0;
const BODY_SIZE: f32 = 44.0;
const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const MARGIN: egui::Vec2 = egui::vec2(32.0, 32.0);

fn setup_egui(
    mut contexts: EguiContexts,
    app_font: Res<AppFont>,
    fonts: Res<Assets<Font>>,
    mut done: Local<bool>,
) -> Result {
    if *done {
        return Ok(());
    }
    let Some(font) = fonts.get(&app_font.0) else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;

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
struct HudState {
    current_texts: HashMap<HudLocation, HudText>,
    show_list: bool,
    /// The search box text. Owned by the [`egui::TextEdit`] in [`render_list`],
    /// which edits it in place; filtering on it comes later.
    list_query: String,
    list_items: Vec<String>,
    /// Index into `list_items` of the highlighted row.
    list_selected: usize,
    /// The list's scroll offset in points, mirrored out of the [`egui::ScrollArea`]
    /// so [`render_list`] can steer it when the selection moves out of view
    /// while leaving the wheel free otherwise.
    list_scroll: f32,
    list_info: String,
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
const SELECTED_ROW_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(64, 64, 64, 64);

fn panel_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(PANEL_FILL)
        .stroke(egui::Stroke::new(PANEL_BORDER, PANEL_STROKE))
        .inner_margin(egui::Margin::same(PANEL_PADDING))
}

/// Draws the file picker: a search box above a scrollable list of
/// [`HudState::list_items`], with a fixed-height info field
/// ([`HudState::list_info`]) below it, centred on screen. The search box takes
/// keyboard focus for as long as the picker is up, with Up/Down/PageUp/PageDown
/// moving the highlighted row. Nothing filters on the query yet, and picking a
/// row does nothing -- those follow when the widget takes over from
/// `crate::fuzzy_list`.
fn render_list(ctx: &egui::Context, state: &mut HudState) {
    if !state.show_list {
        return;
    }
    let screen = ctx.content_rect();
    // Same proportions as the Bevy widget: as wide as the screen is tall (it is
    // opened over a 4:3-ish emulator view), capped to what actually fits.
    let width = screen.height().min(screen.width() - 2.0 * MARGIN.x);
    let inner_width = width - 2.0 * (PANEL_PADDING as f32 + PANEL_BORDER);
    let visible_rows = (screen.height() * LIST_HEIGHT_FRACTION / ROW_HEIGHT)
        .floor()
        .max(1.0);
    let view_height = visible_rows * ROW_HEIGHT;

    // Selection keys are taken before the search box is drawn, so the
    // `TextEdit` never sees them. Home/End are deliberately left alone -- they
    // stay cursor movement inside the query. Counting (rather than testing)
    // the presses keeps a held-down arrow moving at the key repeat rate even
    // when several repeats land in one frame.
    let len = state.list_items.len();
    let (row_steps, page_steps) = ctx.input_mut(|i| {
        let none = egui::Modifiers::NONE;
        let rows = i.count_and_consume_key(none, egui::Key::ArrowDown) as i64
            - i.count_and_consume_key(none, egui::Key::ArrowUp) as i64;
        let pages = i.count_and_consume_key(none, egui::Key::PageDown) as i64
            - i.count_and_consume_key(none, egui::Key::PageUp) as i64;
        (rows, pages)
    });
    let delta = row_steps + page_steps * visible_rows as i64;
    let selected = if len == 0 {
        0
    } else {
        // Clamped rather than wrapped, and re-clamped against the current
        // length in case the list shrank since the last frame.
        (state.list_selected.min(len - 1) as i64 + delta).clamp(0, len as i64 - 1) as usize
    };
    state.list_selected = selected;

    egui::Area::new(egui::Id::new("fuzzy_list"))
        .order(egui::Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = PANEL_GAP;

            panel_frame().show(ui, |ui| {
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

            let items = &state.list_items;
            let mut scroll = egui::ScrollArea::vertical()
                .max_height(view_height)
                .auto_shrink([false, false]);
            if delta != 0 {
                // Follow the selection only when a key actually moved it,
                // scrolling the least that brings it back into view; between
                // keypresses the wheel is left in charge.
                let top = selected as f32 * ROW_HEIGHT;
                let offset = state
                    .list_scroll
                    .min(top)
                    .max(top + ROW_HEIGHT - view_height)
                    .max(0.0);
                scroll = scroll.vertical_scroll_offset(offset);
            }
            let scrolled = panel_frame()
                .show(ui, |ui| {
                    ui.set_width(inner_width);
                    // Rows butt up against each other. This has to be set on
                    // *this* ui, not inside the closure below: `show_rows`
                    // reads `item_spacing.y` here to work out how tall a row is
                    // and which ones the viewport covers, so leaving the
                    // panel's gap in place would have it reserve
                    // `ROW_HEIGHT + PANEL_GAP` per row while we paint them
                    // `ROW_HEIGHT` apart -- the list would come up short at the
                    // bottom and the scroll offsets below would drift by a row
                    // every few rows.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    // Only the rows in view are laid out, so a picker holding
                    // the whole file list stays cheap to draw.
                    scroll.show_rows(ui, ROW_HEIGHT, items.len(), |ui, rows| {
                        for row in rows {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), ROW_HEIGHT),
                                egui::Sense::hover(),
                            );
                            if row == selected {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::CornerRadius::ZERO,
                                    SELECTED_ROW_COLOR,
                                );
                            }
                            // Painted rather than laid out as a `Label`: rows are
                            // single-line and anything too long is clipped at the
                            // scroll area's edge instead of wrapping.
                            ui.painter().text(
                                rect.left_center(),
                                egui::Align2::LEFT_CENTER,
                                &items[row],
                                egui::FontId::proportional(ROW_SIZE),
                                TEXT_COLOR,
                            );
                        }
                    })
                })
                .inner;
            state.list_scroll = scrolled.state.offset.y;

            // The info box stays hidden while there is nothing to say.
            if !state.list_info.is_empty() {
                panel_frame().show(ui, |ui| {
                    ui.set_width(inner_width);
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
}

fn update_ui(
    mut contexts: EguiContexts,
    mut state: ResMut<HudState>,
    time: Res<Time>,
    _window: Single<&mut Window, With<PrimaryWindow>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;

    let rect = ctx.content_rect().shrink2(MARGIN);

    egui::Area::new(egui::Id::new("overlay"))
        .fixed_pos(rect.min)
        .order(egui::Order::Background)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            for (text, corner) in [
                (HudLocation::TopLeft, egui::Align2::LEFT_TOP),
                (HudLocation::TopRight, egui::Align2::RIGHT_TOP),
                (HudLocation::BottomLeft, egui::Align2::LEFT_BOTTOM),
                (HudLocation::InfoText, egui::Align2::RIGHT_BOTTOM),
            ] {
                let info = state.current_texts.get(&text).cloned().unwrap_or_default();
                let on = info.duration.contains(&time.elapsed_secs());
                let t = ctx.animate_bool_with_time(egui::Id::new(corner), on, 1.0);
                if t > 0.0 {
                    let quad = egui::Rect::from_two_pos(corner.pos_in_rect(&rect), rect.center());
                    let layout = if corner.y() == egui::Align::Min {
                        egui::Layout::top_down(corner.x())
                    } else {
                        egui::Layout::bottom_up(corner.x())
                    };

                    let color = ui.visuals().text_color().linear_multiply(t);

                    ui.scope_builder(egui::UiBuilder::new().max_rect(quad).layout(layout), |ui| {
                        ui.heading(egui::RichText::new(info.text).color(color));
                    });
                }
            }
        });

    render_list(ctx, &mut state);
    Ok(())
}

fn spawn_toast(
    mut state: ResMut<HudState>,
    time: Res<Time>,
    mut reader: MessageReader<SetHudText>,
) {
    state.show_list = true;
    state.list_info = "Bottom  info".into();
    if state.list_items.is_empty() {
        for i in 0..100 {
            state.list_items.push(format!("Item{i}"));
        }
    }

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
            .add_systems(Update, spawn_toast.run_if(on_message::<SetHudText>))
            .insert_resource(HudState::default());
    }
}
