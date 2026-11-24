//! A modal box with centered text and an OK button, for telling the user
//! something they have to acknowledge, and optionally a button for acting on it.

use crate::dialog::{BODY_SIZE, ROW_SPACING, scale_widgets};
use crate::{show_panel, take_key};

#[derive(Default)]
pub struct MessageBox {
    text: Option<String>,
    button: Option<String>,
}

impl MessageBox {
    pub fn open(&mut self, text: impl Into<String>) {
        self.text = Some(text.into());
        self.button = None;
    }

    pub fn open_with(&mut self, text: impl Into<String>, button: impl Into<String>) {
        self.text = Some(text.into());
        self.button = Some(button.into());
    }

    pub fn is_open(&self) -> bool {
        self.text.is_some()
    }

    /// Returns true when the extra button was clicked.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let Some(text) = &self.text else {
            return false;
        };
        let mut acted = false;
        let mut closing =
            ctx.input_mut(|i| take_key(i, egui::Key::Enter) + take_key(i, egui::Key::Escape) > 0);
        egui::Area::new(egui::Id::new("message_box"))
            .order(egui::Order::Tooltip)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                show_panel(ui, |ui| {
                    scale_widgets(ui);
                    ui.vertical_centered(|ui| {
                        for line in text.lines() {
                            ui.label(egui::RichText::new(line).size(BODY_SIZE));
                        }
                        ui.add_space(ROW_SPACING.y * 2.0);
                        if let Some(button) = &self.button {
                            acted = ui.button(button.as_str()).clicked();
                            closing |= acted;
                        }
                        closing |= ui.button("OK").clicked();
                    });
                });
            });
        if closing {
            self.text = None;
        }
        acted
    }
}

#[cfg(test)]
#[path = "tests/message_box_tests.rs"]
mod tests;
