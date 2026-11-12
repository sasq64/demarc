//! The render world's side of the frame conversion: runs `frameconv` for every
//! emulator with a pair of source frames, for the composite pass to pick up.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

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

/// Why RIFE could not be used, handed from the render world to the main one.
#[derive(Resource, Clone, Default)]
struct RifeFailure(Arc<Mutex<Option<String>>>);

/// A RIFE frame in a texture of the source's size.
pub(crate) struct RifeTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    frame: Option<Arc<Vec<u32>>>,
}

impl RifeTarget {
    fn new(device: &wgpu::Device, width: u32, height: u32) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("rife frame"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&default());
        Self {
            texture,
            view,
            frame: None,
        }
    }

    fn show(&mut self, queue: &wgpu::Queue, frame: &Arc<Vec<u32>>) {
        if self.frame.as_ref().is_some_and(|f| Arc::ptr_eq(f, frame)) {
            return;
        }
        let (width, height) = (self.texture.width(), self.texture.height());
        queue.write_texture(
            self.texture.as_image_copy(),
            crate::backend::frame_bytes(frame),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.frame = Some(Arc::clone(frame));
    }
}

pub(crate) enum ConvTarget {
    Simple(Box<Converter>),
    Rife(RifeTarget),
}

impl ConvTarget {
    fn size(&self) -> (u32, u32) {
        match self {
            Self::Simple(conv) => conv.size(),
            Self::Rife(rife) => (rife.texture.width(), rife.texture.height()),
        }
    }

    /// The picture to show as it is.
    pub fn six_view(&self) -> &wgpu::TextureView {
        match self {
            Self::Simple(conv) => conv.six_view(),
            Self::Rife(rife) => &rife.view,
        }
    }

    /// The picture at the source's size, for an effect.
    pub fn native_view(&self) -> &wgpu::TextureView {
        match self {
            Self::Simple(conv) => conv.native_view(),
            Self::Rife(rife) => &rife.view,
        }
    }

    pub fn native(&self) -> &wgpu::Texture {
        match self {
            Self::Simple(conv) => conv.native(),
            Self::Rife(rife) => &rife.texture,
        }
    }
}

/// The converted picture of each emulator that has one, by its source image.
/// An emulator not in `shown` this frame shows its source image.
#[derive(Resource, Default)]
pub(crate) struct ConvTargets {
    targets: HashMap<AssetId<Image>, ConvTarget>,
    shown: HashSet<AssetId<Image>>,
}

impl ConvTargets {
    pub fn get(&self, id: &AssetId<Image>) -> Option<&ConvTarget> {
        self.targets.get(id).filter(|_| self.shown.contains(id))
    }
}

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
    pairs.rife = settings.rife;
    if settings.no_motion {
        return;
    }
    for emu in &emus {
        if let Some((a, b, n)) = emu.conversion_pair() {
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

    fn ready(&mut self, failure: &RifeFailure) -> Option<&mut Worker<AssetId<Image>>> {
        let (status, error) = self.worker.status();
        if status != self.status {
            self.status = status;
            match status {
                Status::Ready => info!("frameconv: interpolating with RIFE"),
                Status::Failed => *failure.0.lock().unwrap() = Some(error.unwrap_or_default()),
                Status::Loading => {}
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
    failure: Res<RifeFailure>,
) {
    let device = device.wgpu_device();
    let wanted = |id: &AssetId<Image>, size: (u32, u32)| {
        pairs.pairs.iter().any(|(i, a, b, _)| {
            i == id && convertible(a, b) && (a.width as u32, a.height as u32) == size
        })
    };
    targets.shown.clear();
    targets
        .targets
        .retain(|id, target| wanted(id, target.size()));
    if pairs.pairs.is_empty() {
        return;
    }
    if pairs.rife {
        let rife = rife.get_or_insert_with(RifeState::start).ready(&failure);
        if let Some(rife) = rife {
            show_rife_frames(&pairs, rife, device, &queue, &mut targets);
        }
        return;
    }
    let pipelines = pipelines.get_or_insert_with(|| Pipelines::new(device));
    let mut encoder = device.create_command_encoder(&default());
    for (id, a, b, n) in &pairs.pairs {
        let (width, height) = (a.width as u32, a.height as u32);
        if !convertible(a, b) || !frameconv::supported(device, width, height) {
            continue;
        }
        let target = targets.targets.entry(*id).or_insert_with(|| {
            ConvTarget::Simple(Box::new(Converter::new(device, pipelines, width, height)))
        });
        let ConvTarget::Simple(conv) = target else {
            continue;
        };
        let n = frameconv::sixths(*n);
        conv.render(pipelines, &queue, &mut encoder, &a.pixels, &b.pixels, n);
        targets.shown.insert(*id);
    }
    queue.submit([encoder.finish()]);
}

/// Asks RIFE for the frames still to come, and shows the ones it has made. A
/// frame it has not made shows the source.
fn show_rife_frames(
    pairs: &ExtractedPairs,
    rife: &mut Worker<AssetId<Image>>,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    targets: &mut ConvTargets,
) {
    let ahead = pairs.ahead.iter().filter(|(_, a, b, _)| convertible(a, b));
    rife.keep(
        ahead
            .clone()
            .map(|(id, a, b, phases)| (*id, &a.pixels, &b.pixels, phases.as_slice())),
    );
    for (id, a, b, phases) in ahead {
        let phases: Vec<f32> = phases.iter().copied().filter(|&t| t > 0.0).collect();
        let size = (a.width as u32, a.height as u32);
        rife.request(*id, &a.pixels, &b.pixels, size, &phases);
    }
    for (id, a, b, n) in &pairs.pairs {
        if !convertible(a, b) || *n == 0.0 {
            continue;
        }
        let Some(frame) = rife.get(*id, &a.pixels, &b.pixels, *n) else {
            debug!("frameconv: RIFE frame at {n} not ready");
            continue;
        };
        let (width, height) = (a.width as u32, a.height as u32);
        let target = targets
            .targets
            .entry(*id)
            .or_insert_with(|| ConvTarget::Rife(RifeTarget::new(device, width, height)));
        let ConvTarget::Rife(target) = target else {
            continue;
        };
        target.show(queue, &frame);
        targets.shown.insert(*id);
    }
}

fn exit_on_rife_failure(failure: Res<RifeFailure>, mut exit: MessageWriter<AppExit>) {
    if let Some(error) = failure.0.lock().unwrap().take() {
        crate::println(format!(
            "** Error: --frame-convert=rife could not be used: {error}"
        ));
        exit.write(AppExit::error());
    }
}

impl Plugin for FrameConvPlugin {
    fn build(&self, app: &mut App) {
        let failure = RifeFailure::default();
        app.insert_resource(failure.clone())
            .add_systems(Update, exit_on_rife_failure);
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .insert_resource(failure)
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
