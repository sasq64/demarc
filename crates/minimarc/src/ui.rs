//! The egui side: the picker over the navigator tree, and the corner texts.

use std::time::{Duration, Instant};

use database::emu_file::{EmuFile, bytes_in_progress};
use indexmap::IndexMap;
use navigator::{Navigator, setup_navigator};
use retro_ui::{Hud, HudLocation, Picker, TitleClick};

const FONT: &[u8] = include_bytes!("../../../system/font.ttf");

/// What a key bar button does.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Button {
    F1,
    F3,
    F5,
    F7,
    Space,
    Return,
    Keyboard,
    Joy,
    Disk,
    Reset,
    Pause,
    Warp,
    Prev,
    Next,
    Back,
    Stop,
}

/// The key bar's buttons, eight on the first row.
pub const BUTTONS: [Button; 16] = {
    use Button::*;
    [
        F1, F3, F5, F7, Space, Return, Keyboard, Joy, Disk, Stop, Reset, Pause, Warp, Prev, Next,
        Back,
    ]
};

const BACK_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xb8, 0x6c);

impl Button {
    fn label(self, paused: bool) -> &'static str {
        match self {
            Button::F1 => "F1",
            Button::F3 => "F3",
            Button::F5 => "F5",
            Button::F7 => "F7",
            Button::Space => "\u{f1050}",
            Button::Return => "\u{f0311}",
            Button::Keyboard => "\u{f030c}",
            Button::Joy => "\u{f0297}",
            Button::Back => "",
            Button::Stop => "",
            Button::Disk => "\u{f0249}",
            Button::Reset => "\u{f0709}",
            Button::Pause if paused => "\u{f04b}",
            Button::Pause => "\u{f04c}",
            Button::Warp => "\u{f050}",
            Button::Next => "\u{f051}",
            Button::Prev => "\u{f048}",
        }
    }
}
const KEY_SIZE: f32 = 26.0;
const HUD_SCALE: f32 = 0.35;
/// Physical pixels from the top and right edges to the info text.
const INFO_MARGIN: f32 = 50.0;

pub enum Action {
    Play(EmuFile),
}

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
    /// Index into `BUTTONS` of the button a finger is on.
    pub key_held: Option<usize>,
    /// One per `BUTTONS`.
    key_rects: Vec<egui::Rect>,
    pub paused: bool,
    info_text: String,
    info_shown: bool,
}

impl Ui {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        retro_ui::apply_style(&ctx, FONT.to_vec());
        let mut picker = Picker::default();
        picker.buttons = vec!["".into(), "󰍹".into()];
        picker.dir_buttons = vec!["".into(), "󰍹".into()];
        picker.button_colors = vec![egui::Color32::from_rgb(0x33, 0xdd, 0x55), BACK_COLOR];
        picker.dir_button_colors = vec![BACK_COLOR];
        picker.info_lines = 2.0;
        picker.right_icons = false;
        picker.compact = true;
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
            info_text: String::new(),
            info_shown: false,
        }
    }

    fn now(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Browse `files`, starting inside the database rather than at a root
    /// list with one row in it.
    pub fn set_db(&mut self, name: String, files: &'static [EmuFile]) -> anyhow::Result<()> {
        let dbs: IndexMap<String, &'static [EmuFile]> = [(name.clone(), files)].into();
        setup_navigator(&dbs, &mut self.navigator, &retro_core::system_dir())?;
        self.navigator.go_root().enter(&[&name]);
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

    /// The release `d` steps from the playing one in the list it was picked from.
    pub fn step(&mut self, d: isize) -> Option<EmuFile> {
        let (file, _) = self.navigator.current_launch.as_mut()?.step(d, false)?;
        Some(file)
    }

    pub fn status(&mut self, text: &str) {
        self.status_for(text, Duration::from_secs(3600));
    }

    /// What is playing, in place of the status.
    pub fn info(&mut self, text: &str) {
        self.info_text = text.to_owned();
        self.status("");
        self.set_info(false);
    }

    /// Shows the info text while the key bar is.
    fn set_info(&mut self, show: bool) {
        self.info_shown = show;
        let text = if show {
            self.info_text.clone()
        } else {
            String::new()
        };
        let now = self.now();
        self.hud.set_text(
            HudLocation::InfoText,
            &text,
            now,
            Duration::ZERO,
            Duration::from_secs(3600 * 24 * 365),
        );
    }

    fn status_for(&mut self, text: &str, time: Duration) {
        let now = self.now();
        self.hud
            .set_text(HudLocation::BottomLeft, text, now, Duration::ZERO, time);
    }

    pub fn error(&mut self, text: &str) {
        self.status("");
        let now = self.now();
        self.hud.set_text(
            HudLocation::Error,
            text,
            now,
            Duration::ZERO,
            Duration::from_secs(8),
        );
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
    }

    fn show_changed(&mut self) {
        if let Some(list) = self.navigator.take_show() {
            self.show(list);
        }
    }

    /// Index into `BUTTONS` of the key bar button at a physical position.
    pub fn key_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.keys_open || self.picker.is_open() {
            return None;
        }
        let scale = self.ctx.pixels_per_point();
        let pos = egui::pos2(x as f32 / scale, y as f32 / scale);
        let gap = self.ctx.global_style().spacing.item_spacing.x / 2.0;
        self.key_rects
            .iter()
            .position(|r| r.expand(gap).contains(pos))
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
            for row in BUTTONS.chunks(8) {
                let width = ctx.content_rect().width() / row.len() as f32 - gap;
                let size = egui::vec2(width, KEY_SIZE * 2.0);
                ui.horizontal(|ui| {
                    for &button in row {
                        let mut text = egui::RichText::new(button.label(self.paused))
                            .font(retro_ui::picker_font(ctx, KEY_SIZE));
                        if button == Button::Back {
                            text = text.color(BACK_COLOR);
                        }
                        let button = egui::Button::new(text)
                            .selected(self.key_held == Some(self.key_rects.len()))
                            .min_size(size);
                        self.key_rects.push(ui.add(button).rect);
                    }
                });
            }
        });
    }

    /// Height of the key bar, in points, while it is open.
    pub fn key_bar_height(&self) -> f32 {
        if !self.keys_open || self.picker.is_open() {
            return 0.0;
        }
        let gap = self.ctx.global_style().spacing.item_spacing.x;
        let rect = self
            .key_rects
            .iter()
            .fold(egui::Rect::NOTHING, |a, r| a.union(*r));
        if rect.is_positive() {
            rect.height() + gap
        } else {
            0.0
        }
    }

    /// Draw one frame, and return what the user asked for.
    pub fn draw(&mut self, ctx: &egui::Context) -> Option<Action> {
        let margin = INFO_MARGIN / ctx.pixels_per_point();
        let place = ctx.viewport_rect();
        let place = place
            .with_max_x(place.max.x - margin)
            .with_min_y(place.min.y + margin);
        self.hud.info_place = Some((place, egui::Align2::RIGHT_TOP));
        let show = self.keys_open && !self.picker.is_open();
        if show != self.info_shown {
            self.set_info(show);
        }
        self.hud.show(ctx, self.now(), bytes_in_progress(), None);
        if !self.picker.is_open() {
            if self.keys_open {
                self.draw_keys(ctx);
            }
            return None;
        }
        if let Some(list) = self.navigator.current()
            && let Some(query) = self.picker.query(list.id)
        {
            let (query, selected) = (query.to_owned(), self.picker.selected_item(list.id));
            self.navigator.remember_state(&query, selected);
        }
        if let Some(click) = self.picker.take_title_click() {
            match click {
                TitleClick::Depth(depth) => self.navigator.to_depth(depth),
                TitleClick::Back => self.navigator.back(),
                TitleClick::Forward => self.navigator.forward(),
            };
            self.show_changed();
        }
        match self.picker.take_button_click() {
            Some(1) => {
                self.close();
                return None;
            }
            Some(0) => self.picker.pick_selected(),
            _ => {}
        }
        let picked = self.picker.show(ctx, |_, _, _| vec![]);
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
                Some(Action::Play(file))
            }
            None => {
                self.navigator.enter(&[&picked.text]);
                self.show_changed();
                None
            }
        }
    }
}
