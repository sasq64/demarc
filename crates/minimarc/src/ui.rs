//! The egui side: the picker over the navigator tree, and the corner texts.

use std::time::{Duration, Instant};

use database::emu_file::{EmuFile, bytes_in_progress};
use indexmap::IndexMap;
use navigator::{Navigator, setup_navigator};
use retro_ui::{Hud, HudLocation, Picker, TitleClick};

const FONT: &[u8] = include_bytes!("../../../system/font.ttf");

/// The key bar's buttons, a row of four at a time.
pub const KEY_LABELS: [&str; 8] = [
    "F1", "F3", "F5", "F7", "\u{f07e}", "\u{f07d}", "STOP", "HOME",
];
const KEY_SIZE: f32 = 26.0;
const HUD_SCALE: f32 = 0.35;

pub enum Action {
    Play(EmuFile),
    FlipDisk,
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
    /// Index into `KEY_LABELS` of the button a finger is on.
    pub key_held: Option<usize>,
    key_rects: Vec<egui::Rect>,
}

impl Ui {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        retro_ui::apply_style(&ctx, FONT.to_vec());
        let mut picker = Picker::default();
        picker.buttons = vec!["\u{f04b}".into(), "\u{f0c7}".into()];
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
            key_held: None,
            key_rects: Vec::new(),
        }
    }

    fn now(&self) -> f32 {
        self.start.elapsed().as_secs_f32()
    }

    /// Browse `files`, starting inside the database rather than at a root
    /// list with one row in it.
    pub fn set_db(&mut self, name: String, files: &'static [EmuFile]) -> anyhow::Result<()> {
        let dbs: IndexMap<String, &'static [EmuFile]> = [(name.clone(), files)].into();
        setup_navigator(&dbs, &mut self.navigator)?;
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

    pub fn note(&mut self, text: &str) {
        self.status_for(text, Duration::from_secs(3));
    }

    /// What is playing, in place of the status.
    pub fn info(&mut self, text: &str) {
        self.status("");
        let now = self.now();
        self.hud.set_text(
            HudLocation::InfoText,
            text,
            now,
            Duration::ZERO,
            Duration::from_secs(5),
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

    /// Index into `KEY_LABELS` of the key bar button at a physical position.
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
        let size = egui::vec2(ctx.content_rect().width() / 4.0 - gap, KEY_SIZE * 2.0);
        egui::Area::new(egui::Id::new("key_bar"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -gap))
            .show(ctx, |ui| {
                for row in KEY_LABELS.chunks(4) {
                    ui.horizontal(|ui| {
                        for label in row {
                            let text = egui::RichText::new(*label)
                                .font(retro_ui::picker_font(ctx, KEY_SIZE));
                            let button = egui::Button::new(text)
                                .selected(self.key_held == Some(self.key_rects.len()))
                                .min_size(size);
                            self.key_rects.push(ui.add(button).rect);
                        }
                    });
                }
            });
    }

    /// Draw one frame, and return what the user asked for.
    pub fn draw(&mut self, ctx: &egui::Context) -> Option<Action> {
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
        let mut flip = false;
        match self.picker.take_button_click() {
            Some(0) => self.picker.pick_selected(),
            Some(1) => flip = true,
            _ => {}
        }
        let picked = self.picker.show(ctx, |_, _, _| vec![]);
        if self.picker.take_query_click() {
            self.soft_keyboard = true;
        } else if ctx.input(|i| i.pointer.any_click()) {
            self.soft_keyboard = false;
        }
        if flip {
            return Some(Action::FlipDisk);
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
