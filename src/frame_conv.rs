//! The render world's side of the frame conversion: runs `frameconv` for every
//! emulator with a pair of source frames, for the composite pass to pick up.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        renderer::{RenderDevice, RenderQueue},
    },
};
use frameconv::{Converter, Pipelines};
use rife::{Rife, Status, Worker};

use crate::backend::VideoFrame;
use crate::config::RenderSettings;
use crate::emulator::Emulator;

pub struct FrameConvPlugin;

/// The converted picture of each emulator that has one, by its source image.
#[derive(Resource, Default)]
pub(crate) struct ConvTargets(pub HashMap<AssetId<Image>, Converter>);

#[derive(Resource, Default)]
struct ExtractedPairs {
    pairs: Vec<(AssetId<Image>, VideoFrame, VideoFrame, f32)>,
    /// With RIFE: the pairs from each emulator's current one on, and their phases.
    ahead: Vec<(AssetId<Image>, VideoFrame, VideoFrame, Vec<f32>)>,
    rife: bool,
}

fn extract_pairs(
    mut pairs: ResMut<ExtractedPairs>,
    emus: Extract<Query<&Emulator>>,
    settings: Extract<Res<RenderSettings>>,
) {
    pairs.pairs.clear();
    pairs.ahead.clear();
    pairs.rife = settings.rife && !settings.no_motion;
    for emu in &emus {
        if let Some((a, b, n)) = emu.conversion_pair() {
            let n = if settings.no_motion { 0.0 } else { n };
            pairs.pairs.push((emu.image.id(), a.clone(), b.clone(), n));
        }
        if pairs.rife {
            for (a, b, phases) in emu.conversion_ahead() {
                pairs
                    .ahead
                    .push((emu.image.id(), a.clone(), b.clone(), phases));
            }
        }
    }
}

const MODEL: &str = "rife-v4.6";
const MODEL_URL: &str = "https://raw.githubusercontent.com/nihui/rife-ncnn-vulkan/master/models";

/// The model directory, downloading the model into it the first time.
fn rife_model() -> anyhow::Result<PathBuf> {
    let dir = dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("demarc")
        .join(MODEL);
    std::fs::create_dir_all(&dir)?;
    for name in ["flownet.param", "flownet.bin"] {
        let path = dir.join(name);
        if !path.is_file() {
            let url = format!("{MODEL_URL}/{MODEL}/{name}");
            let file = newsys::fetch::fetch_url_with_progress(&url, &|_, _| {})?;
            std::fs::copy(file, &path)?;
        }
    }
    Ok(dir)
}

/// The RIFE worker and the last status it was seen in.
struct RifeState {
    worker: Worker<AssetId<Image>>,
    status: Status,
}

impl RifeState {
    fn start() -> Self {
        Self {
            worker: Worker::start(|| Rife::load(&rife_model()?)),
            status: Status::Loading,
        }
    }

    fn ready(&mut self) -> Option<&mut Worker<AssetId<Image>>> {
        let (status, error) = self.worker.status();
        if status != self.status {
            self.status = status;
            match status {
                Status::Ready => info!("frameconv: interpolating with RIFE"),
                _ => warn!(
                    "frameconv: no RIFE, using simple motion: {}",
                    error.unwrap_or_default()
                ),
            }
        }
        (status == Status::Ready).then_some(&mut self.worker)
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
    mut rife: Local<Option<RifeState>>,
) {
    let device = device.wgpu_device();
    let wanted = |id: &AssetId<Image>, size: (u32, u32)| {
        pairs.pairs.iter().any(|(i, a, b, _)| {
            i == id && convertible(a, b) && (a.width as u32, a.height as u32) == size
        })
    };
    targets.0.retain(|id, conv| wanted(id, conv.size()));
    if pairs.pairs.is_empty() {
        return;
    }
    let pipelines = pipelines.get_or_insert_with(|| Pipelines::new(device));
    let mut rife = pairs
        .rife
        .then(|| rife.get_or_insert_with(RifeState::start).ready())
        .flatten();
    if let Some(rife) = &mut rife {
        let ahead = pairs.ahead.iter().filter(|(_, a, b, _)| convertible(a, b));
        rife.keep(
            ahead
                .clone()
                .map(|(id, a, b, _)| (*id, &a.pixels, &b.pixels)),
        );
        for (id, a, b, phases) in ahead {
            let phases: Vec<f32> = phases.iter().copied().filter(|&t| t > 0.0).collect();
            let size = (a.width as u32, a.height as u32);
            rife.request(*id, &a.pixels, &b.pixels, size, &phases);
        }
    }
    let mut encoder = device.create_command_encoder(&default());
    for (id, a, b, n) in &pairs.pairs {
        let (width, height) = (a.width as u32, a.height as u32);
        if !convertible(a, b) || !frameconv::supported(device, width, height) {
            continue;
        }
        let conv = targets
            .0
            .entry(*id)
            .or_insert_with(|| Converter::new(device, pipelines, width, height));
        let frame = match &rife {
            Some(_) if *n == 0.0 => Some(Arc::clone(&a.pixels)),
            Some(rife) => rife.get(*id, &a.pixels, &b.pixels, *n),
            None => None,
        };
        if rife.is_some() && frame.is_none() {
            debug!("frameconv: RIFE frame at {n} not ready");
        }
        match frame {
            Some(frame) => conv.present(pipelines, &queue, &mut encoder, &frame),
            None => {
                let n = frameconv::sixths(*n);
                conv.render(pipelines, &queue, &mut encoder, &a.pixels, &b.pixels, n);
            }
        }
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
