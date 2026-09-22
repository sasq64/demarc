//! The librashader side: one filter chain per emulator source per preset, built
//! off the render thread, rendered into an intermediate texture the composite
//! pass then samples. The only module that knows librashader exists.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use bevy::{
    asset::AssetId,
    image::Image,
    prelude::*,
    render::{
        render_resource::{
            CommandEncoder, Extent3d, Texture, TextureDescriptor, TextureDimension, TextureUsages,
            TextureView, TextureViewDescriptor,
        },
        renderer::{RenderDevice, RenderQueue},
    },
    tasks::{AsyncComputeTaskPool, Task, futures::check_ready},
};
use librashader::presets::ShaderFeatures;
use librashader::runtime::wgpu::{FilterChain, WgpuOutputView};
use librashader::runtime::{Size, Viewport};

use super::{ShaderPath, TARGET_FORMAT};

/// A librashader intermediate render target: the emulator framebuffer with the
/// filter chain applied, at display resolution and preserving the source aspect
/// ratio. Sized to the [`ScaleMode::Fit`](super::ScaleMode::Fit) image rectangle
/// of the view; recreated when that size changes (window resize / core
/// resolution change).
struct IntermediateTarget {
    size: UVec2,
    view: TextureView,
}

/// Create an [`IntermediateTarget`] of `size`, into which librashader renders
/// and which the composite blit then samples.
fn build_target(device: &RenderDevice, size: UVec2) -> IntermediateTarget {
    let texture = device.create_texture(&TextureDescriptor {
        label: Some("slang_intermediate"),
        size: Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&TextureViewDescriptor::default());
    IntermediateTarget { size, view }
}

/// Which of a source's presets a view wants to run. There is no passthrough
/// variant: a view that wants neither of these skips librashader entirely (see
/// [`post_process_pass`](super::composite::post_process_pass)).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChainKind {
    /// The visible effect preset (CRT/LCD), selected on the command line.
    Effect,
    /// The DREZ downsample preset, run instead of the effect when the view
    /// magnifies the source less than `--downsample`.
    Downsample,
}

/// What [`SlangChains::render`] left for the composite pass to sample.
pub(super) enum ChainOutput<'a> {
    /// The intermediate the chain rendered into.
    Filtered(&'a TextureView),
    /// Nothing to render with — the first build for this source is still
    /// compiling, or its preset failed to load. Composite the emulator
    /// framebuffer unshaded rather than dropping the view for a second or two.
    Unfiltered,
    /// The chain errored; skip the view for this frame.
    Failed,
}

/// What a background build produced.
enum BuildResult {
    /// The chain the build was asked for.
    Built(Box<FilterChain>),
    /// The preset could not be loaded. Reported by the task; remembered by
    /// [`SlangChains::failed`] so it is neither retried nor re-logged.
    Failed,
    /// The selection moved on before the task got to run, so nothing was
    /// compiled. Distinct from `Failed`: the preset is not blamed for it.
    Cancelled,
}

/// One filter-chain build, running on the async compute pool.
struct Build {
    /// Preset being compiled.
    path: PathBuf,
    /// Set once the selection has moved on. The task checks it before starting,
    /// so a build still queued behind other sources' builds costs nothing; once
    /// glslang has the shaders there is no way to interrupt it (the same caveat
    /// [`crate::jobs`] documents), so a build already running just finishes and
    /// has its result dropped.
    cancelled: Arc<AtomicBool>,
    task: Task<BuildResult>,
    /// When the build was spawned, for the debug line it logs on arrival.
    started: Instant,
    /// Frames the view drew while this build ran, for that same line: the whole
    /// point of building off the render thread is that this is not 1.
    frames: usize,
}

impl Build {
    /// Start compiling `path` off the render thread.
    fn spawn(path: PathBuf, device: &RenderDevice, queue: &RenderQueue) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        // wgpu's device and queue are reference-counted handles and creating
        // resources through them from another thread is supported, so the whole
        // load — glslang, naga, LUT decode, pipeline creation — runs on the
        // worker. The `device.poll(Wait)` librashader ends with blocks that
        // worker only.
        let device = device.wgpu_device().clone();
        let queue = queue.clone();
        debug!("building chain for {}", path.display());
        let task = {
            let path = path.clone();
            let cancelled = Arc::clone(&cancelled);
            AsyncComputeTaskPool::get().spawn(async move {
                // Not an async body in any real sense: nothing below awaits,
                // the blocking compile just occupies one pool thread until it
                // returns.
                if cancelled.load(Ordering::Relaxed) {
                    return BuildResult::Cancelled;
                }
                #[allow(clippy::result_large_err)]
                let loaded =
                    FilterChain::load_from_path(&path, ShaderFeatures::NONE, &device, &queue, None);
                match loaded {
                    Ok(chain) => BuildResult::Built(Box::new(chain)),
                    Err(err) => {
                        error!("failed to load preset {}: {err}", path.display());
                        BuildResult::Failed
                    }
                }
            })
        };
        Self {
            path,
            cancelled,
            task,
            started: Instant::now(),
            frames: 0,
        }
    }
}

/// A filter chain, built off the render thread and swapped in when it is ready.
///
/// Building one is expensive and unbudgeted: a Mega Bezel preset is 42 GLSL
/// passes, and glslang alone takes ~1.5 s of CPU to turn them into SPIR-V
/// (naga and the wgpu pipelines are ~0.1 s on top, and the 32 LUT images ~0.1 s
/// — the images are not the cost). Running that inline on the render thread is
/// what made a shader change freeze the whole app for a second or two, so it
/// runs on [`AsyncComputeTaskPool`] instead and nothing here ever waits on it:
/// until the new chain lands, the view keeps rendering with the previous one,
/// or composites the emulator framebuffer unshaded if there is no previous one.
#[derive(Default)]
struct AsyncChain {
    /// The chain in use and the preset it was built from. Deliberately kept —
    /// and kept rendering — while a build for a *newer* preset is in flight, so
    /// picking a preset in the shader dialog swaps looks in a single frame
    /// instead of falling back to an unshaded image for two seconds.
    ready: Option<(PathBuf, Box<FilterChain>)>,
    /// The build in flight, at most one per source. Cycling through the dialog
    /// therefore coalesces: the running build is flagged cancelled and dropped
    /// on arrival, and only the selection current *at that point* is compiled
    /// next — intermediate ones are never started. Spawning one build per click
    /// instead would have them fight over the CPU and make the selection the
    /// user actually settled on the slowest of the lot.
    building: Option<Build>,
}

impl AsyncChain {
    /// The chain to render this frame, starting or advancing the build of
    /// `want` as needed. `None` means "nothing to run with yet" — the first
    /// build for this source has not finished, or `want` failed to load — and
    /// the caller composites the source unshaded.
    fn get(
        &mut self,
        want: &Path,
        device: &RenderDevice,
        queue: &RenderQueue,
        failed: &mut HashSet<PathBuf>,
    ) -> Option<&mut FilterChain> {
        // Superseded: tell the task to skip the compile if it has not started.
        if let Some(build) = &self.building
            && build.path.as_path() != want
        {
            build.cancelled.store(true, Ordering::Relaxed);
        }
        // Count the frames this view drew while the build ran — the whole
        // point of building off the render thread — then collect it if it has
        // landed. `check_ready` never blocks.
        let mut finished = None;
        if let Some(build) = &mut self.building {
            build.frames += 1;
            finished = check_ready(&mut build.task);
        }
        if let Some(result) = finished {
            let build = self.building.take().expect("polled above");
            let name = build.path.display();
            match result {
                BuildResult::Built(chain) if build.path.as_path() == want => {
                    debug!(
                        "chain for {name} ready in {:.2?}, {} frames drawn meanwhile",
                        build.started.elapsed(),
                        build.frames
                    );
                    self.ready = Some((build.path.clone(), chain));
                }
                // Built something the user has already switched away from, or
                // skipped because the selection moved on before it started:
                // either way drop it and let the current selection start
                // building below.
                BuildResult::Built(_) => debug!("chain for {name} dropped, selection moved on"),
                BuildResult::Cancelled => debug!("build for {name} skipped, selection moved on"),
                // The task logged why.
                BuildResult::Failed => {
                    failed.insert(build.path.clone());
                }
            }
        }
        let known_bad = failed.contains(want);
        if known_bad {
            // Show the frame unshaded rather than a preset the user did not
            // pick; the failure was logged once, by the build itself.
            self.ready = None;
        }
        if should_start(
            self.ready.as_ref().map(|(path, _)| path.as_path()),
            self.building.as_ref().map(|build| build.path.as_path()),
            want,
            known_bad,
        ) {
            self.building = Some(Build::spawn(want.to_path_buf(), device, queue));
        }
        self.ready.as_mut().map(|(_, chain)| &mut **chain)
    }
}

/// Whether a source should start compiling `want` now, given the preset it has
/// `ready` and the one it is already `building`.
///
/// The `building.is_none()` term is the coalescing rule that keeps rapid
/// changes in the shader dialog cheap. glslang cannot be interrupted once it
/// has the shaders, so a superseded build runs to completion whatever we do;
/// starting the next selection alongside it would just have the two compete for
/// the CPU, and with a click per preset the one the user settles on ends up
/// last in a queue of abandoned work. Waiting instead means at most one build
/// per source is ever in flight, every intermediate selection is skipped
/// outright, and the selection current when the running build lands is the one
/// that gets compiled next.
fn should_start(
    ready: Option<&Path>,
    building: Option<&Path>,
    want: &Path,
    known_bad: bool,
) -> bool {
    !known_bad && building.is_none() && ready != Some(want)
}

/// One emulator's librashader state: its effect and downsample chains, its
/// intermediate target, and its frame counter.
#[derive(Default)]
struct SourceChains {
    effect: AsyncChain,
    downsample: AsyncChain,
    /// Intermediate render target, recreated on resize. `None` until the first
    /// frame that needs one.
    target: Option<IntermediateTarget>,
    /// RetroArch-style frame counter fed to the shaders (feedback/animation).
    /// Only ticks on frames that actually run a chain, so an animated preset
    /// resumes where it left off rather than jumping after a spell with the
    /// effect switched off (during which nothing of it was on screen anyway).
    frame_count: usize,
}

/// Per-source librashader chains and intermediate targets.
///
/// The chains are deliberately **not** shared across sources. A single
/// `FilterChain` keeps internal per-pass framebuffers sized to its last
/// `frame()` call; feeding one differently-sized sources within a frame (as a
/// grid of images with different resolutions does) latches the last size and
/// resamples the next frame's other cells through it — e.g. an 18×18 brush in
/// the grid blurs every other image. One chain per source keeps that state
/// isolated. Each chain is built in the background, the first time a view
/// actually selects it (see [`AsyncChain`]).
///
/// `FilterChainWgpu` owns clones of the wgpu `Device`/`Queue` and is `Send`/`Sync`,
/// so this lives as a render-world resource, accessed via `ResMut`.
#[derive(Resource)]
pub(super) struct SlangChains {
    /// Path of the effect preset (`--shader`/`--slangp`), built per source.
    /// `None` on the WGSL backend, which has no chain to build.
    effect_path: Option<PathBuf>,
    /// Path of the DREZ downsample preset.
    downsample_path: PathBuf,
    /// Magnification below which `downsample_path` replaces the effect;
    /// `0` (from `--downsample 0`) never runs it. See
    /// [`wants_downsample`](super::geometry::wants_downsample).
    pub(super) downsample_limit: f32,
    /// Presets whose build failed, so they are neither retried nor re-logged.
    /// Shared by every source: a broken preset is broken for all of them.
    failed: HashSet<PathBuf>,
    /// One set of chains + target per emulator, keyed by its source image.
    /// A source that has never needed a chain has no entry at all.
    sources: HashMap<AssetId<Image>, SourceChains>,
}

impl SlangChains {
    /// Run the `kind` chain for `source` over `texture` into an intermediate of
    /// `size`, recording into `encoder`, and hand back the intermediate for the
    /// composite pass to sample.
    pub(super) fn render(
        &mut self,
        device: &RenderDevice,
        queue: &RenderQueue,
        encoder: &mut CommandEncoder,
        source: AssetId<Image>,
        texture: &Texture,
        size: UVec2,
        kind: ChainKind,
        params: &HashMap<String, f32>,
    ) -> ChainOutput<'_> {
        let Some((chain, target, frame_count)) = self.chain(device, queue, source, size, kind)
        else {
            return ChainOutput::Unfiltered;
        };
        if kind == ChainKind::Effect {
            nudge_triad(chain, size.x as f32);
            // What the shader dialog has changed, applied after the nudge so an
            // explicit triad size still wins.
            if !params.is_empty() {
                use librashader::runtime::FilterChainParameters;
                chain.parameters().update_parameters(|values| {
                    for (name, value) in params.iter() {
                        if let Some(slot) = values.get_mut::<str>(name) {
                            *slot = *value;
                        }
                    }
                });
            }
        }
        let lr_size = Size::new(size.x, size.y);
        let output = WgpuOutputView::new_from_raw(&target.view, lr_size, TARGET_FORMAT);
        let viewport = Viewport {
            x: 0.0,
            y: 0.0,
            mvp: None,
            output,
            size: lr_size,
        };
        if let Err(err) = chain.frame(texture, &viewport, encoder, *frame_count, None) {
            error!("librashader frame failed: {err}");
            return ChainOutput::Failed;
        }
        *frame_count += 1;
        ChainOutput::Filtered(&target.view)
    }

    /// The `kind` chain for `source`, plus its intermediate target and frame
    /// counter, (re)created at `size`. Returns `None` while the chain is still
    /// compiling in the background and when its preset failed to load, so the
    /// caller composites the source unshaded.
    fn chain(
        &mut self,
        device: &RenderDevice,
        queue: &RenderQueue,
        source: AssetId<Image>,
        size: UVec2,
        kind: ChainKind,
    ) -> Option<(&mut FilterChain, &IntermediateTarget, &mut usize)> {
        // Destructured so the preset paths stay readable while `sources` is
        // borrowed mutably.
        let Self {
            effect_path,
            downsample_path,
            failed,
            sources,
            ..
        } = self;
        let sc = sources.entry(source).or_default();
        let (slot, path) = match kind {
            ChainKind::Effect => (&mut sc.effect, effect_path.as_deref()?),
            ChainKind::Downsample => (&mut sc.downsample, downsample_path.as_path()),
        };
        let chain = slot.get(path, device, queue, failed)?;
        // Only now that there is something to render with is the intermediate
        // worth creating (or resizing, after a window resize or a video mode
        // change).
        if sc.target.as_ref().is_none_or(|target| target.size != size) {
            sc.target = Some(build_target(device, size));
        }
        let target = sc.target.as_ref().expect("just created");
        Some((chain, target, &mut sc.frame_count))
    }

    /// Point the effect chains at a different preset. Nothing is dropped and
    /// nothing blocks: each source notices at its next draw, starts building
    /// the new preset in the background and keeps rendering the old chain until
    /// that build lands (see [`AsyncChain`]). A no-op while the preset is
    /// unchanged, which is every frame but the one the settings dialog changes
    /// it on.
    ///
    /// Only the effect is repointed: the downsample preset is `--downsample`,
    /// so those chains stay valid across a shader change.
    pub(super) fn set_effect(&mut self, path: Option<&Path>) {
        if self.effect_path.as_deref() != path {
            self.effect_path = path.map(Path::to_path_buf);
        }
    }
}

/// crt-royale tiles its phosphor mask at a fixed triad size (default 3px => 24px
/// tiles). When render_width / tile_size is an even integer, a tile boundary
/// lands exactly on the screen center, where the mask's manual frac() tiling has
/// a coordinate discontinuity that duplicates a subpixel (a visible red vertical
/// line). crt-royale's own fix (FIX_DISCONTINUITIES) uses ddx/ddy in a
/// vertex-shared header and won't compile on the slang/glslang path. Instead,
/// nudge the triad size a fraction so the center falls as far as possible from
/// any tile boundary: pick the integer tile size near 24px whose center offset is
/// furthest from a boundary. This keeps triads ~3px (visually identical, mask
/// stays pixel-sharp) and moves the seam off-center. No-op on presets without
/// this parameter.
fn nudge_triad(chain: &mut FilterChain, width: f32) {
    use librashader::runtime::FilterChainParameters;
    let mut best_tile = 24i32;
    let mut best_dist = -1.0f32;
    for tile in 22..=26 {
        let center_coord = width / (2.0 * tile as f32);
        let dist = (center_coord - center_coord.round()).abs(); // 0..=0.5
        if dist > best_dist {
            best_dist = dist;
            best_tile = tile;
        }
    }
    chain
        .parameters()
        .set_parameter_value("mask_triad_size_desired", best_tile as f32 / 8.0);
}

/// Record the `.slangp` preset paths for [`SlangChains`]; the chains themselves
/// are built lazily, once per emulator source and only for the presets a view
/// actually selects (see [`SlangChains::chain`]).
///
/// Inserted even on the WGSL backend, which runs no chains: the settings dialog
/// can switch to a `.slangp` preset later, and there is nothing to build until
/// it does.
pub(super) fn init_filter_chains(mut commands: Commands, shader_path: Res<ShaderPath>) {
    commands.insert_resource(SlangChains {
        effect_path: shader_path.effect.slangp().map(Path::to_path_buf),
        downsample_path: shader_path.downsample.clone(),
        downsample_limit: shader_path.downsample_limit,
        failed: HashSet::new(),
        sources: HashMap::new(),
    });
}

#[cfg(test)]
#[path = "tests/chains_tests.rs"]
mod tests;
