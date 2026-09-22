//! Compositing every emulator view into one camera, through a CRT/LCD shader.
//!
//! - [`geometry`] — where a view's picture lands on screen, and the uniform the
//!   composite shader reads.
//! - [`chains`] — the librashader filter chains, built off the render thread.
//!   Nothing else in the crate touches librashader.
//! - [`composite`] — the render pass that draws one quad per view.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::{
    core_pipeline::{Core2d, Core2dSystems},
    image::Image,
    prelude::*,
    render::{
        RenderApp, RenderStartup,
        extract_component::{ExtractComponent, ExtractComponentPlugin, UniformComponentPlugin},
        extract_resource::{ExtractResource, ExtractResourcePlugin},
        render_resource::{ShaderType, TextureFormat},
    },
};

use crate::config::RenderSettings;

mod chains;
mod composite;
mod geometry;

pub use geometry::view_transform;

/// Format of both the librashader intermediate target and the composite blit's
/// output. Matches Bevy's view-target main texture (formerly
/// `TextureFormat::bevy_default()`).
const TARGET_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;

/// The bundled DREZ downsample preset, relative to the `system` dir. Swapped in
/// for the effect preset on views that magnify the source less than
/// `--downsample` (by default: that minify it).
pub const DOWNSAMPLE_PRESET: &str = "shaders/slangp/downsample/drez_1x.slangp";

/// Asset path of the passthrough composite shader, used on the
/// [`ShaderEffect::Slangp`] backend where the filter chain has already applied
/// the effect into the intermediate this samples.
const BLIT_SHADER: &str = "shaders/blit.wgsl";

/// Which post-process backend to run.
#[derive(Clone, Debug)]
pub enum ShaderEffect {
    /// librashader `.slangp` filter chain, run into an intermediate texture
    /// that the (passthrough) composite blit then draws. An absolute path,
    /// resolved from the `system` dir — or given verbatim by `--slangp`.
    /// With the effect toggled off there is no chain at all: the composite
    /// samples the emulator framebuffer directly (see
    /// [`composite::post_process_pass`]).
    Slangp(PathBuf),
    /// The pre-librashader single-pass WGSL path: one shader asset (e.g.
    /// `shaders/lottes.wgsl`) that samples the emulator framebuffer directly
    /// and applies the effect in the composite pass itself. The
    /// effect/passthrough toggle is handled in-shader via `crt_enabled`.
    Wgsl(String),
}

impl ShaderEffect {
    /// The `.slangp` preset to build a filter chain from, or `None` on the
    /// WGSL backend, which runs no chains.
    fn slangp(&self) -> Option<&Path> {
        match self {
            ShaderEffect::Slangp(path) => Some(path),
            ShaderEffect::Wgsl(_) => None,
        }
    }

    /// Asset path of the shader the composite pass runs: the passthrough blit
    /// behind a filter chain, the effect itself on the WGSL backend.
    fn composite_shader(&self) -> &str {
        match self {
            ShaderEffect::Slangp(_) => BLIT_SHADER,
            ShaderEffect::Wgsl(asset_path) => asset_path,
        }
    }
}

/// The post-process shader in force, chosen on the command line
/// (`--shader`/`--slangp`) and changeable at runtime from the settings dialog.
///
/// [`ExtractResource`] so a change made in the main world reaches the render
/// world, which rebuilds whatever it invalidated: the composite pipeline in
/// [`composite::post_process_pass`] and the filter chains in
/// [`chains::SlangChains::set_effect`]. It is also inserted into the render
/// world directly, so `RenderStartup` — one extract too early to see it — has
/// it.
#[derive(Resource, Clone, ExtractResource)]
pub struct ShaderPath {
    pub effect: ShaderEffect,
    /// DREZ downsample preset, substituted for the effect on views below
    /// `downsample_limit`. Slangp backend only.
    pub downsample: PathBuf,
    /// Magnification (on-screen pixels per source pixel) below which the
    /// downsampler replaces the effect, from `--downsample`. Mirrors
    /// [`AppSettings::crt_limit`](crate::config::AppSettings::crt_limit): at the
    /// default `1.0` the substitution kicks in exactly when the view shows the
    /// source *smaller* than its native resolution; `0` disables the
    /// downsampler entirely.
    pub downsample_limit: f32,
    /// Effect-preset parameter values the shader dialog has changed, applied to
    /// the chain before each frame it draws. Behind an `Arc` because the whole
    /// resource is cloned into the render world once a frame.
    pub params: Arc<HashMap<String, f32>>,
}

pub struct PostProcessPlugin {
    /// The backend (and shader/preset paths) to run.
    pub shader: ShaderPath,
}

impl Plugin for PostProcessPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.shader.clone())
            .add_plugins((
                ExtractResourcePlugin::<RenderSettings>::default(),
                ExtractResourcePlugin::<ShaderPath>::default(),
                ExtractComponentPlugin::<PostProcess>::default(),
                ExtractComponentPlugin::<PostProcessUniform>::default(),
                ExtractComponentPlugin::<BorderScissor>::default(),
                ExtractComponentPlugin::<EmuCamera>::default(),
                UniformComponentPlugin::<PostProcessUniform>::default(),
            ))
            .add_systems(PostUpdate, geometry::update_post_process_uniform);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Seed the render world's copy: `RenderStartup` runs before the first
        // extract, so the systems below would otherwise have nothing to read.
        // From then on `ExtractResourcePlugin` keeps it in step with the main
        // world's, which the settings dialog writes to.
        render_app.insert_resource(self.shader.clone());

        // Bevy 0.19 replaced the render graph with schedule-driven rendering: a
        // render pass is just a system in the per-camera `Core2d` schedule. We run
        // in the `PostProcess` set (where tonemapping lives), which is ordered
        // before upscaling presents the view target to the swapchain.
        render_app
            .add_systems(
                RenderStartup,
                (composite::init_blit_pipeline, chains::init_filter_chains),
            )
            .add_systems(
                Core2d,
                composite::post_process_pass.in_set(Core2dSystems::PostProcess),
            );
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum ScaleMode {
    /// Fill the window, distorting the source aspect ratio.
    Stretch,
    /// Preserve the source aspect ratio with letterbox/pillarbox bars.
    #[default]
    Fit,
    /// Preserve the source aspect ratio by scaling to fill the window and
    /// cropping the overflow (top/bottom or left/right).
    Zoom,
    /// Scale the source by a fixed factor `n` and centre the result in the
    /// window. Square-pixel systems get `n`×`n` screen pixels; the core's
    /// reported aspect corrects non-square pixels (e.g. Amiga half-width →
    /// 2n×n). Whole-number factors keep pixels integer-sized for crispness;
    /// fractional factors (e.g. 2.5) are applied exactly. If the scaled image
    /// is larger than the window it overflows and is cropped.
    Fixed(f32),
}

/// How the shader samples outside the source image (in the letterbox/pillarbox
/// bars, or anywhere `warp` pushes a fetch off the edge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum BorderMode {
    /// `ClampToEdge`: replicate the nearest edge texel, so edge pixels stretch
    /// outward into the border.
    Stretch,
    /// `ClampToBorder` with a black border color: sample black outside the
    /// image, giving a clean black border.
    #[default]
    Black,
}

#[derive(Component, Clone, ExtractComponent)]
pub struct PostProcess {
    pub source: Handle<Image>,
    //pub scale_mode: ScaleMode,
    /// Display aspect ratio (width / height) the core wants the frame shown at.
    /// When `<= 0`, the source texture's pixel dimensions are used instead.
    pub aspect: f32,
    /// Manual multiplier applied on top of `aspect` for fine correction (1.0 = none).
    pub aspect_tweak: f32,
    /// The centred part of the source texture that holds the picture, from
    /// [`Backend::get_used_frame_size`](crate::backend::Backend::get_used_frame_size).
    /// Zero, or the whole texture, means there is no border to crop.
    pub used: UVec2,
    pub view: ViewRect,
    /// Opacity of the view over the clear color: `0` skips it, `1` draws it unblended.
    pub alpha: f32,
    /// Composite the source as it is: no effect, no downsampler. What the DJ
    /// window shows, where a filter chain would be a second one over the same
    /// source texture.
    pub raw: bool,
    // How the border (outside the source image) is sampled.
    // pub border_mode: BorderMode,
}

#[derive(Component, Default, Clone, Copy, PartialEq, ExtractComponent, ShaderType)]
pub struct PostProcessUniform {
    uv_scale: Vec2,
    uv_offset: Vec2,
    /// `1` when the CRT effect is active for *this view*, `0` for a plain
    /// passthrough blit. Decided per view in `geometry::compute_uniform`: the
    /// global [`RenderSettings::crt_effect`] toggle, further gated by
    /// [`AppSettings::crt_limit`](crate::config::AppSettings::crt_limit)
    /// against this view's magnification. The slangp backend reads it back off
    /// the extracted component to decide whether to run a filter chain at all,
    /// so both backends agree on a single decision.
    crt_enabled: u32,
}

/// Pixel rectangle (in physical framebuffer coords) that the post-process blit
/// is restricted to with a scissor, so the letterbox/pillarbox bars are left
/// untouched and show the camera's clear color instead of being overdrawn.
///
/// `None` means "no clipping" — the blit covers the whole viewport. That's used
/// for [`BorderMode::Stretch`], where the bars are intentionally filled with
/// stretched edge texels by the shader rather than the clear color.
#[derive(Component, Clone, Copy, PartialEq, Eq, ExtractComponent)]
pub struct BorderScissor(pub Option<URect>);

/// The window rectangle, in physical pixels, that one emulator view draws into.
///
/// Grid mode used to give every cell its own `Camera2d` with a `viewport`, so
/// the entire 2D pipeline — extraction, view uniforms, main pass, tonemapping,
/// upscaling, one render pass each — ran once per cell. A grid now has a single
/// camera and every view is a plain entity contributing one quad to
/// [`composite::post_process_pass`]; this is what that camera viewport used to
/// be, and is written from the window size by the frontend.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ViewRect {
    /// Top-left corner in physical pixels.
    pub position: UVec2,
    /// Size in physical pixels. Zero until the first frame has sized it.
    pub size: UVec2,
    /// `false` while a *different* view is maximized over the window: the quad
    /// is skipped entirely, exactly as an inactive camera used to be.
    pub active: bool,
}

impl ViewRect {
    /// The rectangle in physical framebuffer coordinates, or `None` before the
    /// first frame has sized it (or while the window is degenerate).
    pub fn rect(&self) -> Option<URect> {
        (self.active && self.size.x > 0 && self.size.y > 0)
            .then(|| URect::from_corners(self.position, self.position + self.size))
    }
}

/// Marks the one camera the emulator views composite into. Its
/// [`ViewTarget`](bevy::render::view::ViewTarget) is what
/// [`composite::post_process_pass`] draws every quad to — the UI camera, which
/// renders the HUD on top afterwards, must not carry this.
#[derive(Component, Clone, Copy, ExtractComponent)]
pub struct EmuCamera;
