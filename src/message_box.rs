//! The Bevy side of [`retro_ui::message_box`]: [`ShowMessage`] opens it.

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};

use crate::egui_ui::{HudState, update_ui};

#[derive(Message)]
pub struct ShowMessage(pub String);

#[derive(Resource, Default)]
struct MessageBox(retro_ui::message_box::MessageBox);

pub struct MessageBoxPlugin;

impl Plugin for MessageBoxPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MessageBox>()
            .add_message::<ShowMessage>()
            .add_systems(Update, open_box.run_if(on_message::<ShowMessage>))
            .add_systems(EguiPrimaryContextPass, message_box_ui.after(update_ui));
    }
}

fn open_box(
    mut reader: MessageReader<ShowMessage>,
    mut mb: ResMut<MessageBox>,
    mut hud: ResMut<HudState>,
) {
    let Some(msg) = reader.read().last() else {
        return;
    };
    if !mb.0.is_open() {
        hud.set_settings_open(true);
    }
    mb.0.open(msg.0.clone());
}

fn message_box_ui(
    mut contexts: EguiContexts,
    mut mb: ResMut<MessageBox>,
    mut hud: ResMut<HudState>,
) -> Result {
    if !mb.0.is_open() {
        return Ok(());
    }
    mb.0.show(contexts.ctx_mut()?);
    if !mb.0.is_open() {
        hud.set_settings_open(false);
    }
    Ok(())
}
