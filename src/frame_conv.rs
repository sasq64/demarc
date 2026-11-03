//! The render world's side of the frame conversion: runs `frameconv` for every
//! emulator with a pair of source frames, for the composite pass to pick up.

use std::collections::HashMap;

use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        renderer::{RenderDevice, RenderQueue},
    },
};
use frameconv::{Converter, Pipelines};

use crate::backend::VideoFrame;
use crate::config::RenderSettings;
use crate::emulator::Emulator;

pub struct FrameConvPlugin;

/// The converted picture of each emulator that has one, by its source image.
#[derive(Resource, Default)]
pub(crate) struct ConvTargets(pub HashMap<AssetId<Image>, Converter>);

#[derive(Resource, Default)]
struct ExtractedPairs(Vec<(AssetId<Image>, VideoFrame, VideoFrame, u32)>);

fn extract_pairs(
    mut pairs: ResMut<ExtractedPairs>,
    emus: Extract<Query<&Emulator>>,
    settings: Extract<Res<RenderSettings>>,
) {
    pairs.0.clear();
    for emu in &emus {
        if let Some((a, b, n)) = emu.conversion_pair() {
            let n = if settings.no_motion { 0 } else { n };
            pairs.0.push((emu.image.id(), a.clone(), b.clone(), n));
        }
    }
}

/// A resolution change leaves a pair that cannot be interpolated; the picture
/// then falls back to the source until both frames are in the new size.
fn convertible(a: &VideoFrame, b: &VideoFrame) -> bool {
    a.width > 0
        && a.height > 0
        && (a.width, a.height) == (b.width, b.height)
        && a.pixels.len() >= a.width * a.height
        && b.pixels.len() >= a.width * a.height
}

#[hotpath::measure]
fn convert_frames(
    pairs: Res<ExtractedPairs>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut targets: ResMut<ConvTargets>,
    mut pipelines: Local<Option<Pipelines>>,
) {
    let device = device.wgpu_device();
    let wanted = |id: &AssetId<Image>, size: (u32, u32)| {
        pairs.0.iter().any(|(i, a, b, _)| {
            i == id && convertible(a, b) && (a.width as u32, a.height as u32) == size
        })
    };
    targets.0.retain(|id, conv| wanted(id, conv.size()));
    if pairs.0.is_empty() {
        return;
    }
    let pipelines = pipelines.get_or_insert_with(|| Pipelines::new(device));
    let mut encoder = device.create_command_encoder(&default());
    for (id, a, b, n) in &pairs.0 {
        let (width, height) = (a.width as u32, a.height as u32);
        if !convertible(a, b) || !frameconv::supported(device, width, height) {
            continue;
        }
        let conv = targets
            .0
            .entry(*id)
            .or_insert_with(|| Converter::new(device, pipelines, width, height));
        conv.render(pipelines, &queue, &mut encoder, &a.pixels, &b.pixels, *n);
    }
    queue.submit([encoder.finish()]);
}

impl Plugin for FrameConvPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<ConvTargets>()
            .init_resource::<ExtractedPairs>()
            .add_systems(ExtractSchedule, extract_pairs)
            .add_systems(
                Render,
                convert_frames
                    .in_set(RenderSystems::PrepareResources)
                    .after(RenderSystems::PrepareAssets),
            );
    }
}
