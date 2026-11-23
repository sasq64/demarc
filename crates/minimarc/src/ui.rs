//! The egui side: the picker over the navigator tree, and the corner texts.

use std::time::{Duration, Instant};

use database::emu_file::{EmuFile, bytes_in_progress};
use database::playlists::Playlists;
use indexmap::IndexMap;
use navigator::{FAVORITES, Navigator, setup_navigator, setup_phone};
use retro_core::libretro;
use retro_ui::message_box::MessageBox;
use retro_ui::{Hud, HudLocation, Picker, TitleClick};

const FONT: &[u8] = include_bytes!("../../../system/font.ttf");

/// What a key bar button does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    F1,
    F3,
    F5,
    F7,
    MouseLeft,
    MouseRight,
    Tab,
    Esc,
    Space,
    Return,
    Keyboard,
    /// The release's web page.
    Csdb,
    Disk,
    Reset,
    Pause,
    Warp,
    Prev,
    Next,
    Back,
    Stop,
    /// The heart left of the info text.
    Favorite,
}

/// The key bar's buttons.
const ROWS: [&[Button]; 2] = {
    use Button::*;
    [
        &[F1, F3, F5, F7, Space, Return, Keyboard, Csdb],
        &[Disk, Stop, Reset, Pause, Warp, Prev, Next, Back],
    ]
};

const AMIGA_ROWS: [&[Button]; 2] = {
    use Button::*;
    [
        &[
            MouseLeft, MouseRight, Tab, Esc, Space, Return, Keyboard, Csdb,
        ],
        ROWS[1],
    ]
};

const BACK_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xb8, 0x6c);
const WEB_COLOR: egui::Color32 = egui::Color32::from_rgb(0xa0, 0x80, 0xfc);

impl Button {
    /// Whether pressing it closes the key bar.
    pub fn closes_bar(self) -> bool {
        self == Button::Csdb || ROWS[1].contains(&self)
    }

    /// The key it holds down, if it is a key.
    pub fn key(self) -> Option<libretro::retro_key> {
        use libretro::*;
        Some(match self {
            Button::F1 => RETROK_F1,
            Button::F3 => RETROK_F3,
            Button::F5 => RETROK_F5,
            Button::F7 => RETROK_F7,
            Button::Space => RETROK_SPACE,
            Button::Return => RETROK_RETURN,
            Button::Esc => RETROK_ESCAPE,
            Button::Tab => RETROK_TAB,
            _ => return None,
        })
    }

    fn label(self, paused: bool) -> &'static str {
        match self {
            Button::F1 => "F1",
            Button::F3 => "F3",
            Button::F5 => "F5",
            Button::F7 => "F7",
            Button::MouseLeft => "\u{f037d}L",
            Button::MouseRight => "\u{f037d}R",
            Button::Tab => "\u{f0312}",
            Button::Esc => "󱊷",
            Button::Space => "󱁐",
            Button::Return => "󰌑",
            Button::Keyboard => "󰌌",
            Button::Csdb => "󰖟",
            Button::Back => "",
            Button::Stop => "",
            Button::Disk => "󰉉",
            Button::Reset => "󰜉",
            Button::Pause if paused => "",
            Button::Pause => "",
            Button::Warp => "",
            Button::Next => "",
            Button::Prev => "",
            Button::Favorite => HEART_OUTLINE,
        }
    }
}
const HEART: &str = "\u{f02d1}";
const HEART_OUTLINE: &str = "\u{f02d5}";
/// The heart's size over the info text's.
const HEART_SCALE: f32 = 2.0;
const KEY_SIZE: f32 = 26.0;
const HUD_SCALE: f32 = 0.35;
/// Physical pixels from the top and right edges to the info text.
const INFO_MARGIN: f32 = 50.0;
const INFO_TIME: Duration = Duration::from_secs(5);
const FOREVER: Duration = Duration::from_secs(3600 * 24 * 365);

pub struct Ui {
    pub ctx: egui::Context,
    picker: Picker<EmuFile>,
    hud: Hud,
    navigator: Navigator,
    start: Instant,
    /// Set by a tap on the search box, until a tap elsewhere.
    pub soft_keyboard: bool,
    pub keys_open: bool,
    /// Where the key bar starts, in points, when it goes under the frame.
    pub bar_top: Option<f32>,
    /// The key bar button a finger is on.
    pub key_held: Option<Button>,
    key_rects: Vec<(Button, egui::Rect)>,
    pub paused: bool,
    /// An Amiga is running: the key bar has the mouse buttons.
    pub amiga: bool,
    /// The picker's shuffle button was clicked; see [`Ui::shuffle`].
    pub shuffle_requested: bool,
    info_text: String,
    info_shown: bool,
    favorites: Option<Playlists>,
    /// The release last handed to the loader.
    pub playing: Option<EmuFile>,
    message: MessageBox,
}

impl Ui {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        retro_ui::apply_style(&ctx, FONT.to_vec());
        let mut picker = Picker::default();
        picker.right_icons = false;
        picker.compact = true;
        picker.phone = true;
        navigator::picker_sources::SHOW_URL.store(false, std::sync::atomic::Ordering::Relaxed);
        Self {
            ctx,
            picker,
            hud: Hud::scaled(HUD_SCALE),
            navigator: Navigator::new(),
            start: Instant::now(),
            soft_keyboard: false,
            keys_open: false,
            bar_top: None,
            key_held: None,
            key_rects: Vec::new(),
            paused: false,
            amiga: false,
            shuffle_requested: false,
            info_text: String::new(),
            info_shown: false,
            favorites: None,
            playing: None,
            message: MessageBox::default(),
        }
    }

    fn now(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Browse `files`, starting inside the database rather than at a root
    /// list with one row in it.
    pub fn set_db(&mut self, dbs: Vec<(String, &'static [EmuFile])>) -> anyhow::Result<()> {
        let name = dbs
            .first()
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        let several = dbs.len() > 1;
        crate::web::set_dbs(dbs.clone());
        let dbs: IndexMap<String, &'static [EmuFile]> = dbs.into_iter().collect();
        setup_phone(&mut self.navigator)?;
        setup_navigator(&dbs, &mut self.navigator, &retro_core::system_dir())?;
        let favorites = Playlists::load(&playlist_dir());
        self.navigator
            .add_playlist(FAVORITES, favorites.lists[0].files);
        self.favorites = Some(favorites);
        let root = self.navigator.go_root();
        if !several {
            root.enter(&[&name]);
        }
        self.status("");
        self.open();
        Ok(())
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_open()
    }

    pub fn open(&mut self) {
        if let Some(list) = self.navigator.current() {
            self.show(list);
        }
    }

    pub fn goto(&mut self, path: Vec<String>) {
        self.navigator.goto_path(path);
        self.show_changed();
    }

    pub fn close(&mut self) {
        self.soft_keyboard = false;
        self.picker.show_list = false;
    }

    pub fn back(&mut self) {
        self.navigator.back();
        self.show_changed();
    }

    pub fn forward(&mut self) {
        self.navigator.forward();
        self.show_changed();
    }

    /// Shuffle the open list and return its first entry, now launched.
    pub fn shuffle(&mut self) -> Option<EmuFile> {
        let (file, _) = self.navigator.shuffle()?;
        self.close();
        Some(file)
    }

    /// The release `d` steps from the playing one in the list it was picked from.
    pub fn step(&mut self, d: isize) -> Option<EmuFile> {
        let (file, _) = self.navigator.current_launch.as_mut()?.step(d, false)?;
        Some(file)
    }

    /// Add the playing release to the favorites, or take it out.
    pub fn toggle_favorite(&mut self) {
        let (Some(favorites), Some(file)) = (self.favorites.as_mut(), self.playing.as_ref()) else {
            return;
        };
        favorites.toggle(0, file, "");
        self.navigator
            .add_playlist(FAVORITES, favorites.lists[0].files);
    }

    fn is_favorite(&self) -> bool {
        let (Some(favorites), Some(file)) = (&self.favorites, &self.playing) else {
            return false;
        };
        favorites.is_favorite(file)
    }

    pub fn status(&mut self, text: &str) {
        self.status_for(text, FOREVER);
    }

    /// What is playing, in place of the status, shown for `INFO_TIME`.
    pub fn info(&mut self, text: &str) {
        self.info_text = text.to_owned();
        self.status("");
        self.info_shown = false;
        self.hud_text(HudLocation::InfoText, text, INFO_TIME);
    }

    /// Shows the info text while the key bar is.
    fn set_info(&mut self, show: bool) {
        self.info_shown = show;
        let text = if show {
            self.info_text.clone()
        } else {
            String::new()
        };
        self.hud_text(HudLocation::InfoText, &text, FOREVER);
    }

    pub fn status_for(&mut self, text: &str, time: Duration) {
        self.hud_text(HudLocation::BottomLeft, text, time);
    }

    pub fn message(&mut self, text: impl Into<String>) {
        self.message.open(text);
    }

    pub fn error(&mut self, text: &str) {
        self.status("");
        self.hud_text(HudLocation::Error, text, Duration::from_secs(8));
    }

    fn hud_text(&mut self, loc: HudLocation, text: &str, time: Duration) {
        let now = self.now();
        self.hud.set_text(loc, text, now, Duration::ZERO, time);
    }

    fn bar_visible(&self) -> bool {
        self.keys_open && !self.picker.is_open()
    }

    fn show(&mut self, list: navigator::ShowList) {
        self.soft_keyboard = false;
        self.picker.open(
            list.id,
            list.source,
            Some(&list.prompt),
            list.selected,
            &list.title,
            list.title_select,
            true,
        );
        self.picker.set_scroll(list.scroll);
    }

    fn show_changed(&mut self) {
        if let Some(list) = self.navigator.take_show() {
            self.show(list);
        }
    }

    /// The key bar button at a physical position.
    pub fn key_at(&self, x: f64, y: f64) -> Option<Button> {
        if !self.bar_visible() {
            return None;
        }
        let scale = self.ctx.pixels_per_point();
        let pos = egui::pos2(x as f32 / scale, y as f32 / scale);
        let gap = self.ctx.global_style().spacing.item_spacing.x / 2.0;
        self.key_rects
            .iter()
            .find(|(_, r)| r.expand(gap).contains(pos))
            .map(|&(b, _)| b)
    }

    fn draw_keys(&mut self, ctx: &egui::Context) {
        self.key_rects.clear();
        let gap = ctx.global_style().spacing.item_spacing.x;
        let area = egui::Area::new(egui::Id::new("key_bar"));
        let area = match self.bar_top {
            Some(top) => area
                .pivot(egui::Align2::CENTER_TOP)
                .fixed_pos(egui::pos2(ctx.content_rect().center().x, top + gap)),
            None => area.anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -gap)),
        };
        area.show(ctx, |ui| {
            let rows = if self.amiga { AMIGA_ROWS } else { ROWS };
            for row in rows {
                let width = ctx.content_rect().width() / row.len() as f32 - gap;
                let size = egui::vec2(width, KEY_SIZE * 2.0);
                ui.horizontal(|ui| {
                    for &button in row {
                        let mut text = egui::RichText::new(button.label(self.paused))
                            .font(retro_ui::picker_font(ctx, KEY_SIZE));
                        if button == Button::Back {
                            text = text.color(BACK_COLOR);
                        } else if button == Button::Csdb {
                            text = text.color(WEB_COLOR);
                        }
                        let widget = egui::Button::new(text)
                            .selected(self.key_held == Some(button))
                            .min_size(size);
                        self.key_rects.push((button, ui.add(widget).rect));
                    }
                });
            }
        });
    }

    /// The favorite toggle, left of the `info` text.
    fn draw_heart(&mut self, ctx: &egui::Context, info: egui::Rect) {
        let theme = retro_ui::theme(ctx);
        let (glyph, color) = if self.is_favorite() {
            (HEART, theme.hud.favorite)
        } else {
            (HEART_OUTLINE, theme.hud.text)
        };
        let size = retro_ui::HEADING_SIZE * HUD_SCALE * HEART_SCALE;
        let pos = info.left_center() - egui::vec2(size * 0.3, 0.0);
        let font = retro_ui::picker_font(ctx, size);
        // Painted from an Area: a bare layer painter's layer is not drawn.
        let rect = egui::Area::new(egui::Id::new("favorite"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::Pos2::ZERO)
            .show(ctx, |ui| {
                ui.painter()
                    .text(pos, egui::Align2::RIGHT_CENTER, glyph, font, color)
            })
            .inner;
        self.key_rects
            .push((Button::Favorite, rect.expand(size * 0.5)));
    }

    /// Height of the key bar, in points, while it is open.
    pub fn key_bar_height(&self) -> f32 {
        if !self.bar_visible() {
            return 0.0;
        }
        let gap = self.ctx.global_style().spacing.item_spacing.x;
        let rect = self
            .key_rects
            .iter()
            .filter(|(b, _)| *b != Button::Favorite)
            .fold(egui::Rect::NOTHING, |a, (_, r)| a.union(*r));
        if rect.is_positive() {
            rect.height() + gap
        } else {
            0.0
        }
    }

    /// Draw one frame, and return the release the user picked.
    pub fn draw(&mut self, ctx: &egui::Context) -> Option<EmuFile> {
        let margin = INFO_MARGIN / ctx.pixels_per_point();
        let place = ctx.viewport_rect();
        let place = place
            .with_max_x(place.max.x - margin)
            .with_min_y(ctx.content_rect().min.y + margin);
        self.hud.info_place = Some((place, egui::Align2::RIGHT_TOP));
        let show = self.bar_visible();
        if show != self.info_shown {
            self.set_info(show);
        }
        self.message.show(ctx);
        let info_rect = self.hud.show(ctx, self.now(), bytes_in_progress(), None);
        if show {
            self.draw_keys(ctx);
            if let Some(info) = info_rect
                && self.playing.is_some()
            {
                self.draw_heart(ctx, info);
            }
        }
        if !self.picker.is_open() {
            return None;
        }
        if let Some(list) = self.navigator.current()
            && let Some(query) = self.picker.query(list.id)
        {
            let (query, selected) = (query.to_owned(), self.picker.selected_item(list.id));
            self.navigator.remember_state(&query, selected);
            self.navigator.remember_scroll(self.picker.scroll());
        }
        if let Some(click) = self.picker.take_title_click() {
            match click {
                TitleClick::Depth(depth) => self.navigator.to_depth(depth),
                TitleClick::Back => self.navigator.back(),
                TitleClick::Forward => self.navigator.forward(),
            };
            self.show_changed();
        }
        let picked = self.picker.show(ctx, |_, _, _| vec![]);
        self.shuffle_requested |= self.picker.take_shuffle_click();
        if self.picker.take_query_click() {
            self.soft_keyboard = true;
        } else if ctx.input(|i| i.pointer.any_click()) {
            self.soft_keyboard = false;
        }
        let picked = picked?;
        match picked.data {
            Some(file) => {
                self.navigator.launch(picked.item);
                self.close();
                Some(file)
            }
            None => {
                self.navigator.enter(&[&picked.text]);
                self.show_changed();
                None
            }
        }
    }
}

/// Where the favorites are kept: the app's data dir on a phone, demarc's
/// playlists elsewhere.
fn playlist_dir() -> std::path::PathBuf {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        retro_core::system_dir().join("playlists")
    } else {
        database::playlists::default_dir()
    }
}

#[cfg(test)]
#[path = "tests/ui_tests.rs"]
mod tests;
