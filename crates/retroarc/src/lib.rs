//! RetroArch `.slangp` filter chains on plain wgpu.
//!
//! The only crate in the tree that knows librashader exists. It owns the
//! expensive part — compiling a preset's passes, off whatever thread the caller
//! renders on — and hands back an intermediate texture; where that texture ends
//! up is the caller's business.

use std::path::Path;

use librashader::preprocess::ShaderSource;
use librashader::presets::{ShaderFeatures, ShaderPreset};
use tracing::warn;

mod chains;

pub use chains::{ChainKind, ChainOutput, Chains};

/// Format of the intermediate a chain renders into. Matches Bevy's view-target
/// main texture, which is what demarc's composite pass then blits from.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// One parameter a preset's passes declare, as the shader dialog shows it.
pub struct PresetParam {
    pub name: String,
    /// The `#pragma parameter` description, which is also the RetroArch menu
    /// label. Never blank — see [`preset_parameters`].
    pub description: String,
    /// The value the chain starts with, with the preset's own `#parameter`
    /// lines already applied over the shader's initial value.
    pub initial: f32,
    pub minimum: f32,
    pub maximum: f32,
    pub step: f32,
    /// Which pass declared it, so the caller can keep the passes' order while
    /// sorting within one.
    pub pass: usize,
}

/// Every parameter the preset's passes declare, deduplicated (a parameter
/// shared by several passes is one entry) and in pass order.
///
/// The values are the ones the chain starts with: the shader's initial value,
/// overridden by the preset's own `#parameter` lines — the same precedence
/// librashader's `RuntimeParameters` applies when it builds the chain.
///
/// A preset that does not parse yields nothing, and says why in the log.
pub fn preset_parameters(path: &Path) -> Vec<PresetParam> {
    let preset = match ShaderPreset::try_parse(path, ShaderFeatures::NONE) {
        Ok(preset) => preset,
        Err(err) => {
            warn!("{}: {err}", path.display());
            return Vec::new();
        }
    };
    let mut params: Vec<PresetParam> = Vec::new();
    for (index, pass) in preset.passes.iter().enumerate() {
        let Ok(source) = ShaderSource::load(&pass.path, preset.features) else {
            continue;
        };
        let declared: Vec<PresetParam> = source
            .parameters
            .values()
            // A pragma with a blank description is one of the spacers the Mega
            // Bezel packs lay their RetroArch menu out with; there is nothing
            // to label a row with.
            .filter(|p| !p.description.trim().is_empty())
            .filter(|p| !params.iter().any(|old| old.name == p.id.as_ref()))
            .map(|p| PresetParam {
                name: p.id.to_string(),
                description: p.description.clone(),
                initial: p.initial,
                minimum: p.minimum,
                maximum: p.maximum,
                step: p.step,
                pass: index,
            })
            .collect();
        params.extend(declared);
    }
    for over in &preset.parameters {
        if let Some(param) = params.iter_mut().find(|p| p.name == over.name.as_ref()) {
            param.initial = over.value;
        }
    }
    params
}

#[cfg(test)]
#[path = "tests/chains_tests.rs"]
mod tests;
