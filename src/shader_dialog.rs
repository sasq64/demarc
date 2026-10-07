//! The Bevy side of the shader dialog ([`retro_ui::shader_dialog`]): opens it on
//! [`ShowShaderDialog`], and writes what it picks straight to [`ShaderPath`],
//! which the render world extracts.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bevy::prelude::*;
use bevy_egui::{EguiContexts, EguiPrimaryContextPass};
use retro_ui::shader_dialog::{ShaderAction, ShaderParam};

use crate::config::{Args, RenderSettings, ShaderArg};
use crate::egui_ui::{HudState, update_ui};
use crate::post_process::{ShaderEffect, ShaderPath};
use crate::system_dir;

/// Opens the shader dialog (RightAlt+Shift+E, [`crate::commands::Cmd`]).
#[derive(Message)]
pub struct ShowShaderDialog;

#[derive(Resource, Default)]
struct ShaderDialog(retro_ui::shader_dialog::ShaderDialog);

pub struct ShaderDialogPlugin;

impl Plugin for ShaderDialogPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ShaderDialog>()
            .add_message::<ShowShaderDialog>()
            .add_systems(Update, open_dialog.run_if(on_message::<ShowShaderDialog>))
            // After `update_ui`, which sets the frame's `pixels_per_point` --
            // the same ordering the settings dialog needs.
            .add_systems(EguiPrimaryContextPass, shader_dialog_ui.after(update_ui));
    }
}

fn preset_of(shader: &ShaderPath) -> Option<&Path> {
    match &shader.effect {
        ShaderEffect::Slangp(path) => Some(path),
        ShaderEffect::Wgsl(_) => None,
    }
}

fn open_dialog(
    mut reader: MessageReader<ShowShaderDialog>,
    mut dialog: ResMut<ShaderDialog>,
    mut hud_state: ResMut<HudState>,
    shader: Res<ShaderPath>,
    args: Res<Args>,
) {
    // One open however many asked for it this frame.
    if reader.read().count() == 0 {
        return;
    }
    if dialog
        .0
        .open(args.shader_dir.as_deref(), system_dir(), preset_of(&shader))
    {
        hud_state.set_settings_open(true);
    }
}

fn shader_dialog_ui(
    mut contexts: EguiContexts,
    mut dialog: ResMut<ShaderDialog>,
    mut hud: ResMut<HudState>,
    mut shader_path: ResMut<ShaderPath>,
    mut render: ResMut<RenderSettings>,
    args: Res<Args>,
) -> Result {
    if !dialog.0.is_open() {
        return Ok(());
    }
    let ctx = contexts.ctx_mut()?;
    let default = args.shader.unwrap_or_default();
    // `--shader none` is the stock passthrough preset with the effect switched
    // off, so name what it does rather than what it does it with.
    let default_label = if default == ShaderArg::None {
        "no effect"
    } else {
        default.path()
    };
    dialog
        .0
        .refresh_params(preset_of(&shader_path), preset_params);

    match dialog.0.show(ctx, default_label) {
        Some(ShaderAction::Preset(preset)) => {
            // The overrides named the old preset's parameters.
            shader_path.params = Arc::new(HashMap::new());
            match preset {
                Some(path) => {
                    shader_path.effect = ShaderEffect::Slangp(path);
                    // Picking a preset is asking to see it, so switch the effect on.
                    render.crt_effect = true;
                }
                None => {
                    shader_path.effect = default.effect();
                    render.crt_effect = default != ShaderArg::None;
                }
            }
            dialog
                .0
                .refresh_params(preset_of(&shader_path), preset_params);
        }
        Some(ShaderAction::Param(name, value)) => {
            Arc::make_mut(&mut shader_path.params).insert(name, value);
        }
        // The chain keeps whatever it was last set to, so an emptied map would
        // leave the overrides in force: set each back to the preset's value.
        Some(ShaderAction::Reset) => {
            if let Some(preset) = preset_of(&shader_path) {
                let defaults = retroarc::preset_parameters(preset);
                let params = Arc::make_mut(&mut shader_path.params);
                for (name, value) in params.iter_mut() {
                    if let Some(p) = defaults.iter().find(|p| &p.name == name) {
                        *value = p.initial;
                    }
                }
            }
        }
        None => {}
    }
    if !dialog.0.is_open() {
        hud.set_settings_open(false);
    }
    Ok(())
}

/// Every parameter the preset's passes declare, deduplicated (a parameter
/// shared by several passes is one row).
///
/// The values are the ones the chain starts with: the shader's initial value,
/// overridden by the preset's own `#parameter` lines -- the same precedence
/// librashader's `RuntimeParameters` applies when it builds the chain.
fn preset_params(path: &Path) -> Vec<ShaderParam> {
    retroarc::preset_parameters(path)
        .into_iter()
        .map(|p| {
            ShaderParam::new(
                p.name,
                &p.description,
                p.initial,
                p.minimum,
                p.maximum,
                p.step,
                p.pass,
            )
        })
        .collect()
}

#[cfg(test)]
#[path = "tests/shader_dialog_tests.rs"]
mod tests;
