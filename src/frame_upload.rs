//! Emulator frames go to the GPU straight from the core's buffer: the render
//! world writes each view's newest frame into its texture, so the `Image`
//! asset is never touched per frame and Bevy never re-extracts it.

use std::collections::HashMap;

use bevy::{
    prelude::*,
    render::{
        Extract, ExtractSchedule, Render, RenderApp, RenderSystems,
        render_asset::RenderAssets,
        render_resource::{
            Extent3d, Origin3d, TexelCopyBufferLayout, TexelCopyTextureInfo, TextureAspect,
            TextureId,
        },
        renderer::{RenderDevice, RenderQueue},
        texture::GpuImage,
    },
};

use crate::backend::{VideoFrame, frame_bytes};
use crate::emulator::Emulator;

pub struct FrameUploadPlugin;

/// The frame each emulator shows this frame, keyed by its texture.
#[derive(Resource, Default)]
struct ExtractedFrames(Vec<(AssetId<Image>, VideoFrame)>);

#[hotpath::measure]
fn extract_frames(mut frames: ResMut<ExtractedFrames>, emus: Extract<Query<&Emulator>>) {
    frames.0.clear();
    for emu in &emus {
        if let Some(frame) = emu.frame_queue.front() {
            frames.0.push((emu.image.id(), frame.clone()));
        }
    }
}

/// The texture a frame from `dmabuf_import` is already in.
#[cfg(target_os = "linux")]
fn gpu_texture(frame: &VideoFrame) -> Option<&wgpu::Texture> {
    let gpu = frame.gpu.as_ref()?;
    Some(&gpu.downcast_ref::<crate::dmabuf_import::GpuTexture>()?.texture)
}

#[cfg(not(target_os = "linux"))]
fn gpu_texture(_frame: &VideoFrame) -> Option<&wgpu::Texture> {
    None
}

/// `uploaded` remembers the last frame written to each texture, so a core that
/// has not produced a new one costs nothing. A resize brings a new texture.
/// Holding on to that frame also keeps a GPU frame's pooled texture from being
/// reused before the copy out of it is submitted.
#[hotpath::measure]
fn upload_frames(
    frames: Res<ExtractedFrames>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
    mut uploaded: Local<HashMap<AssetId<Image>, (TextureId, VideoFrame)>>,
) {
    uploaded.retain(|id, _| frames.0.iter().any(|(i, _)| i == id));
    let mut encoder = None;
    for (id, frame) in &frames.0 {
        let Some(gpu) = gpu_images.get(*id) else {
            continue;
        };
        let texture = gpu.texture.id();
        if uploaded
            .get(id)
            .is_some_and(|(t, f)| *t == texture && VideoFrame::same(f, frame))
        {
            continue;
        }
        // A resize recreates the image, which may not have reached the GPU yet.
        let size = gpu.texture_descriptor.size;
        let width = (frame.width as u32).min(size.width);
        let height = (frame.height as u32).min(size.height);
        let extent = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        if let Some(src) = gpu_texture(frame) {
            let src_size = src.size();
            if width == 0 || height == 0 || src_size.width < width || src_size.height < height {
                continue;
            }
            encoder
                .get_or_insert_with(|| device.wgpu_device().create_command_encoder(&default()))
                .copy_texture_to_texture(
                    src.as_image_copy(),
                    TexelCopyTextureInfo {
                        texture: &gpu.texture,
                        mip_level: 0,
                        origin: Origin3d::ZERO,
                        aspect: TextureAspect::All,
                    },
                    extent,
                );
            uploaded.insert(*id, (texture, frame.clone()));
            continue;
        }
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
            extent,
        );
        uploaded.insert(*id, (texture, frame.clone()));
    }
    if let Some(encoder) = encoder {
        queue.submit([encoder.finish()]);
    }
}

impl Plugin for FrameUploadPlugin {
    fn build(&self, app: &mut App) {
        #[cfg(target_os = "linux")]
        app.add_systems(Startup, crate::dmabuf_import::install);
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
