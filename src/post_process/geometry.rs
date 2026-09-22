//! Where a view's picture lands on screen: the scale modes, the uniform the
//! composite shader reads, and the scissor that keeps the bars clear.

use bevy::{image::Image, prelude::*};

use super::{BorderMode, BorderScissor, PostProcess, PostProcessUniform, ScaleMode, ViewRect};
use crate::config::{AppSettings, RenderSettings};

/// Recomputes both post-process components from the view's current rectangle.
///
/// Nothing here changes unless the window is resized, the core switches video
/// mode or a hotkey toggles the shader — but it runs every frame, so both writes
/// go through `set_if_neq` and the components are only inserted when missing.
/// An unconditional write would mark them changed (and re-queue an archetype
/// insert through `Commands`) on every single frame.
pub(super) fn update_post_process_uniform(
    images: Res<Assets<Image>>,
    settings: Res<RenderSettings>,
    app_settings: Res<AppSettings>,
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &PostProcess,
        Option<&mut PostProcessUniform>,
        Option<&mut BorderScissor>,
    )>,
) {
    for (entity, pp, existing, existing_scissor) in &mut query {
        let (uniform, image) = compute_uniform(pp, &settings, app_settings.crt_limit, &images);
        let scissor = BorderScissor(compute_scissor(image, &settings, &pp.view));
        match existing {
            Some(mut u) => {
                u.set_if_neq(uniform);
            }
            None => {
                commands.entity(entity).insert(uniform);
            }
        }
        match existing_scissor {
            Some(mut s) => {
                s.set_if_neq(scissor);
            }
            None => {
                commands.entity(entity).insert(scissor);
            }
        }
    }
}

/// Clip rectangle for the blit so the bars keep the clear color. Returns `None`
/// (no clipping) for [`BorderMode::Stretch`], or when the image fills the whole
/// viewport (`Stretch`/`Zoom` scaling), so we never scissor away visible pixels.
fn compute_scissor(
    image: (Vec2, Vec2),
    settings: &RenderSettings,
    view_rect: &ViewRect,
) -> Option<URect> {
    let (image_scale, image_offset) = image;
    if !matches!(settings.border_mode, BorderMode::Black) {
        return None;
    }
    let rect = view_rect.rect()?;
    let vp_min = rect.min.as_vec2();
    let vp_size = rect.size().as_vec2();
    // The image occupies screen-uv `[uv_offset, uv_offset + uv_scale]` within the
    // viewport; the bars are whatever falls outside that. Clamp to the viewport so
    // Zoom/Stretch (which push the image past the edges) just yield the full rect.
    let img_min = (vp_min + image_offset * vp_size).max(vp_min);
    let img_max = (vp_min + (image_offset + image_scale) * vp_size).min(vp_min + vp_size);
    if img_max.x <= img_min.x || img_max.y <= img_min.y {
        return None;
    }
    Some(URect::from_corners(
        img_min.round().as_uvec2(),
        img_max.round().as_uvec2(),
    ))
}

/// The uniform for one view, and where the picture itself lands in screen-uv —
/// which is only the same thing when there is no border to crop.
fn compute_uniform(
    pp: &PostProcess,
    settings: &RenderSettings,
    crt_limit: f32,
    images: &Assets<Image>,
) -> (PostProcessUniform, (Vec2, Vec2)) {
    // Use this view's rectangle, not the whole window: in grid mode every cell
    // gets its own sub-rect of the one camera, so aspect must be computed
    // against that quadrant. A single emulator's rect is the whole window.
    let viewport = pp.view.rect().map(|r| r.size());
    let src = images.get(&pp.source).map(|source| source.size());
    let (mut uv_scale, mut uv_offset) = match (viewport, src) {
        (Some(target), Some(src)) => view_transform(
            target,
            src,
            pp.used,
            pp.aspect,
            pp.aspect_tweak,
            settings.scale_mode,
        ),
        _ => (Vec2::ONE, Vec2::ZERO),
    };
    // Snap the composite transform to the intermediate's integer pixel grid so
    // the passthrough blit samples it exactly 1:1 (identity texel mapping).
    //
    // `composite::post_process_pass` sizes the intermediate texture to
    // `inter_size = round(viewport * uv_scale)`, but the image's on-screen
    // footprint is the *fractional* `viewport * uv_scale`. That sub-texel
    // mismatch makes the nearest-sampled blit drift up to half a texel across the
    // image and, by the centring symmetry, duplicate one column at the exact
    // screen centre — a visible mask-phase discontinuity (the thin red vertical
    // line under crt-lottes at "Fit"). Forcing the footprint to that integer
    // `inter_size` (and an integer top-left) makes `(screen_uv - uv_offset) /
    // uv_scale` land on exact texel centres: one screen pixel per intermediate
    // texel, no seam.
    if let Some(target) = viewport {
        let target = target.as_vec2();
        let inter = (uv_scale * target).round().max(Vec2::ONE);
        uv_scale = inter / target;
        uv_offset = (uv_offset * target).round() / target;
    }
    // The effect is on only if it's globally enabled *and* this view magnifies
    // the source enough for the scanlines/mask to resolve. Both the viewport and
    // the source size are per-view, so a grid cell can fall below the limit
    // while the same core, maximized, stays above it.
    let crt_enabled = settings.crt_effect
        && !pp.raw
        && match (viewport, src) {
            (Some(target), Some(src)) => pixel_ratio(target, src, uv_scale) >= crt_limit,
            // Source not loaded yet: keep the global setting rather than
            // flickering the effect off for a frame.
            _ => true,
        };
    // Undo the crop, on the snapped values so the bars line up with the pixels
    // actually drawn: what is left is the picture's own rectangle.
    let used = used_fraction(src.unwrap_or(UVec2::ONE), pp.used);
    let image = (uv_scale * used, uv_offset + uv_scale * (1.0 - used) * 0.5);
    (
        PostProcessUniform {
            uv_scale,
            uv_offset,
            crt_enabled: crt_enabled as u32,
        },
        image,
    )
}

/// How much of the source texture is picture rather than border, per axis.
fn used_fraction(src: UVec2, used: UVec2) -> Vec2 {
    if src.x == 0 || src.y == 0 || used.x == 0 || used.y == 0 {
        return Vec2::ONE;
    }
    (used.as_vec2() / src.as_vec2()).clamp(Vec2::splat(f32::EPSILON), Vec2::ONE)
}

/// [`scale_offset`] for a source that carries a border: a wine release running
/// 4:3 inside a 16:9 gamescope session arrives as a session sized frame with the
/// picture scaled into the middle of it, and the scale modes have to work on the
/// picture rather than on the frame.
///
/// So the modes are given the picture's size and display aspect, and the
/// transform they return is then widened to sample only the picture — screen-uv
/// `[0,1]` over the image maps to the centred `used` sub-range of the texture
/// instead of all of it. With no border this is exactly [`scale_offset`].
pub fn view_transform(
    target: UVec2,
    src: UVec2,
    used: UVec2,
    aspect: f32,
    aspect_tweak: f32,
    scale_mode: ScaleMode,
) -> (Vec2, Vec2) {
    let f = used_fraction(src, used);
    if f == Vec2::ONE {
        return scale_offset(target, src, aspect, aspect_tweak, scale_mode);
    }
    // `aspect` is the whole frame's; cropping to the picture changes it by the
    // same ratio the crop does.
    let aspect = if aspect > 0.0 {
        aspect * f.x / f.y
    } else {
        0.0
    };
    let (scale, offset) = scale_offset(target, used, aspect, aspect_tweak, scale_mode);
    let scale = scale / f;
    (scale, offset - scale * (1.0 - f) * 0.5)
}

/// How many screen pixels the source gets per source pixel in this view, taken
/// on the *tighter* of the two axes.
///
/// `uv_scale ⊙ target` is the image's on-screen footprint in pixels (the same
/// `inter_size` the librashader intermediate is sized to), so dividing by the
/// source dimensions gives the magnification per axis. The two differ whenever
/// the pixel aspect isn't square — an Amiga half-width frame is stretched twice
/// as far horizontally as vertically — and the effect is limited by whichever
/// axis has the least room, so take the minimum.
fn pixel_ratio(target: UVec2, src: UVec2, uv_scale: Vec2) -> f32 {
    if src.x == 0 || src.y == 0 {
        return f32::INFINITY;
    }
    (uv_scale * target.as_vec2() / src.as_vec2()).min_element()
}

/// Whether a view whose on-screen footprint is `inter` should run the
/// downsampler: it magnifies the source by less than `limit` on at least one
/// axis. Taken per axis rather than on the aspect-corrected magnification,
/// because a frame stretched on one axis and squeezed on the other (a
/// half-width Amiga screen shown at 1:1) still aliases on the squeezed one.
///
/// `limit <= 0` never downsamples; the default `1.0` is exactly "the view
/// throws source pixels away".
pub(super) fn wants_downsample(inter: UVec2, src: UVec2, limit: f32) -> bool {
    if limit <= 0.0 || src.x == 0 || src.y == 0 {
        return false;
    }
    (inter.as_vec2() / src.as_vec2()).min_element() < limit
}

/// The letterbox/pillarbox transform for showing a `src`-sized source in a
/// `target`-sized viewport under `scale_mode`. The shader (and the pointer
/// mapping in `retro.rs`) map a screen-uv to the source with
/// `(screen_uv - uv_offset) / uv_scale`; this returns `(uv_scale, uv_offset)`.
/// `Stretch`, a degenerate size, or a target that already matches the source
/// aspect all yield the identity transform.
pub fn scale_offset(
    target: UVec2,
    src: UVec2,
    aspect: f32,
    aspect_tweak: f32,
    scale_mode: ScaleMode,
) -> (Vec2, Vec2) {
    if matches!(scale_mode, ScaleMode::Stretch)
        || target.x == 0
        || target.y == 0
        || src.x == 0
        || src.y == 0
    {
        return (Vec2::ONE, Vec2::ZERO);
    }
    // Exact integer scaling, aspect-aware. The core's reported display aspect
    // divided by the framebuffer's own pixel dimensions gives the pixel aspect
    // ratio (PAR): how wide each source pixel should appear relative to its
    // height. Square-pixel systems (Game Boy 160×144 @ ~1.11, etc.) give
    // PAR≈1 → `n`×`n` pixels; an Amiga half-width framebuffer gives PAR≈2 →
    // 2n×n; half-height gives PAR≈0.5 → n×2n. We keep the factor on the denser
    // axis at exactly `n` and multiply the other by the PAR, rounded so pixels
    // stay integer-sized (and therefore crisp). The result is centred.
    if let ScaleMode::Fixed(n) = scale_mode {
        let base_aspect = if aspect > 0.0 {
            aspect
        } else {
            src.x as f32 / src.y as f32
        };
        let par = (base_aspect * aspect_tweak) / (src.x as f32 / src.y as f32);
        // Whole-number factors round the aspect-corrected axis so pixels stay
        // integer-sized (crisp). A fractional factor is an explicit request for
        // non-integer scaling, so honour it exactly on both axes.
        let snap = |v: f32| if n.fract() == 0.0 { v.round() } else { v }.max(1.0);
        let (hx, hy) = if par >= 1.0 {
            (snap(n * par), n)
        } else {
            (n, snap(n / par))
        };
        let footprint = Vec2::new(src.x as f32 * hx, src.y as f32 * hy);
        let scale = footprint / target.as_vec2();
        return (scale, (Vec2::ONE - scale) * 0.5);
    }
    let target_aspect = target.x as f32 / target.y as f32;
    // Use the display aspect ratio the core reports; fall back to the texture's
    // pixel dimensions when the core doesn't supply one.
    let base_aspect = if aspect > 0.0 {
        aspect
    } else {
        src.x as f32 / src.y as f32
    };
    let source_aspect = base_aspect * aspect_tweak;

    let mut sm = scale_mode;
    if (target_aspect - source_aspect).abs() < 0.02 {
        sm = ScaleMode::Zoom;
    }

    let target_wider = target_aspect > source_aspect;
    let scale = match (sm, target_wider) {
        (ScaleMode::Stretch, _) => Vec2::ONE,
        // Fit: shrink the source uv range on the constrained axis, leaving bars.
        (ScaleMode::Fit, true) => Vec2::new(source_aspect / target_aspect, 1.0),
        (ScaleMode::Fit, false) => Vec2::new(1.0, target_aspect / source_aspect),
        // Zoom: expand the screen-uv-to-source-uv ratio on the cropped axis,
        // so screen uv [0,1] maps to a sub-range of the source.
        (ScaleMode::Zoom, true) => Vec2::new(1.0, target_aspect / source_aspect),
        (ScaleMode::Zoom, false) => Vec2::new(source_aspect / target_aspect, 1.0),
        // `Fixed` and `Stretch` returned early above; unreachable here.
        (ScaleMode::Fixed(_), _) => Vec2::ONE,
    };
    (
        scale,
        Vec2::new((1.0 - scale.x) * 0.5, (1.0 - scale.y) * 0.5),
    )
}

#[cfg(test)]
#[path = "tests/geometry_tests.rs"]
mod tests;
