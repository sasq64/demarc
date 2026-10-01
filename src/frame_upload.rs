//! Emulator frames go to the GPU straight from the core's buffer: the render
//! world writes each view's newest frame into its texture, so the `Image`
//! asset is never touched per frame and Bevy never re-extracts it.

use std::collections::HashMap;
use std::sync::Arc;

use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        render_asset::RenderAssets,
        render_resource::{
            Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
            TextureId,
        },
        renderer::RenderQueue,
        texture::GpuImage,
    },
};

use crate::backend::{VideoFrame, frame_bytes};
use crate::emulator::Emulator;

pub struct FrameUploadPlugin;

/// The frame each emulator shows this frame, keyed by its texture.
#[derive(Resource, Default)]
struct ExtractedFrames(Vec<(AssetId<Image>, VideoFrame)>);

fn extract_frames(mut frames: ResMut<ExtractedFrames>, emus: Extract<Query<&Emulator>>) {
    frames.0.clear();
    for emu in &emus {
        if let Some(frame) = emu.frame_queue.front() {
            frames.0.push((emu.image.id(), frame.clone()));
        }
    }
}

/// `uploaded` remembers the last frame written to each texture, so a core that
/// has not produced a new one costs nothing. A resize brings a new texture.
fn upload_frames(
    frames: Res<ExtractedFrames>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    queue: Res<RenderQueue>,
    mut uploaded: Local<HashMap<AssetId<Image>, (TextureId, Arc<Vec<u32>>)>>,
) {
    uploaded.retain(|id, _| frames.0.iter().any(|(i, _)| i == id));
    for (id, frame) in &frames.0 {
        let Some(gpu) = gpu_images.get(*id) else {
            continue;
        };
        let texture = gpu.texture.id();
        if uploaded
            .get(id)
            .is_some_and(|(t, p)| *t == texture && Arc::ptr_eq(p, &frame.pixels))
        {
            continue;
        }
        // A resize recreates the image, which may not have reached the GPU yet.
        let size = gpu.texture_descriptor.size;
        let width = (frame.width as u32).min(size.width);
        let height = (frame.height as u32).min(size.height);
        if width == 0 || height == 0 || frame.pixels.len() < frame.width * frame.height {
            continue;
        }
        queue.write_texture(
            TexelCopyTextureInfo {
                texture: &gpu.texture,
                mip_level: 0,
                origin: Origin3d::ZERO,
                aspect: TextureAspect::All,
            },
            frame_bytes(&frame.pixels),
            TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(frame.width as u32 * 4),
                rows_per_image: None,
            },
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        uploaded.insert(*id, (texture, Arc::clone(&frame.pixels)));
    }
}

impl Plugin for FrameUploadPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app
            .init_resource::<ExtractedFrames>()
            .add_systems(ExtractSchedule, extract_frames)
            .add_systems(
                Render,
                upload_frames
                    .in_set(RenderSystems::PrepareResources)
                    .after(RenderSystems::PrepareAssets),
            );
    }
}
