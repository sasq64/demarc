//! Offscreen rendering for `--headless`.
//!
//! With no window there is nothing for the cameras to draw into and nothing for
//! `screenshot()` to read back, so both target this image instead. Its size is
//! what the frontend lays views out against, standing in for the window size.

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::{image::Image, prelude::*};

use wgpu::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

/// Size of the offscreen target, in pixels.
const SIZE: UVec2 = UVec2::new(1280, 720);

/// The image everything renders into when there is no window. Present only
/// under `--headless`, so its absence is what the rest of the code tests.
#[derive(Resource)]
pub struct HeadlessTarget {
    pub image: Handle<Image>,
    pub size: UVec2,
}

impl HeadlessTarget {
    pub fn new(images: &mut Assets<Image>) -> Self {
        let mut image = Image::new_fill(
            Extent3d {
                width: SIZE.x,
                height: SIZE.y,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            &[0, 0, 0, 255],
            // sRGB, like the window surface the shader chain otherwise ends in.
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
            | TextureUsages::COPY_SRC
            | TextureUsages::RENDER_ATTACHMENT;
        Self {
            image: images.add(image),
            size: SIZE,
        }
    }
}

/// What a camera should render to: the offscreen image when headless, the
/// primary window otherwise.
pub fn camera_target(target: Option<&HeadlessTarget>) -> RenderTarget {
    match target {
        Some(t) => RenderTarget::Image(t.image.clone().into()),
        None => RenderTarget::default(),
    }
}
