use super::*;

/// The magnification the `crt_limit` check sees for showing `src` in a
/// `target`-sized viewport under `mode`, with square source pixels.
fn ratio(target: UVec2, src: UVec2, mode: ScaleMode) -> f32 {
    let (uv_scale, _) = scale_offset(target, src, 0.0, 1.0, mode);
    pixel_ratio(target, src, uv_scale)
}

#[test]
fn ratio_is_the_on_screen_magnification() {
    let src = UVec2::new(320, 240);
    // Exactly 2x — the boundary a `crt_limit` of 2.0 tests against.
    assert!((ratio(UVec2::new(640, 480), src, ScaleMode::Fit) - 2.0).abs() < 1e-4);
    // Pillarboxed in a wider window: the constrained axis still sets it.
    assert!((ratio(UVec2::new(1280, 480), src, ScaleMode::Fit) - 2.0).abs() < 1e-4);
    // A grid cell of a 1920x1080 window at 5x4 lands below 2x.
    assert!(ratio(UVec2::new(384, 270), src, ScaleMode::Fit) < 2.0);
    // Maximized to the whole window, the same core is above it.
    assert!(ratio(UVec2::new(1920, 1080), src, ScaleMode::Fit) >= 2.0);
}

#[test]
fn fixed_scale_ratio_matches_the_factor() {
    let r = ratio(
        UVec2::new(1920, 1080),
        UVec2::new(320, 240),
        ScaleMode::Fixed(3.0),
    );
    assert!((r - 3.0).abs() < 1e-4);
}

/// Non-square source pixels stretch the axes differently; the check takes
/// the tighter one (here vertical, on a half-width Amiga frame).
#[test]
fn ratio_uses_the_tighter_axis() {
    let src = UVec2::new(320, 256);
    let r = ratio(UVec2::new(1280, 512), src, ScaleMode::Fixed(2.0));
    assert!((r - 2.0).abs() < 1e-4);
}

/// The bundled downsample preset has to parse and reference a shader that
/// is actually in the `system` tree — a preset that only fails at
/// `FilterChain::load_from_path` time shows up as a log line at runtime and
/// silently leaves minified views unfiltered.
#[test]
fn bundled_downsample_preset_resolves() {
    use librashader::presets::ShaderPreset;
    let path = crate::system_dir().join(DOWNSAMPLE_PRESET);
    let preset = ShaderPreset::try_parse(&path, ShaderFeatures::NONE)
        .unwrap_or_else(|err| panic!("{path:?} should parse: {err}"));
    let pass = preset.passes.first().expect("preset should have a pass");
    assert!(pass.path.is_file(), "missing shader {:?}", pass.path);
}

/// The on-screen footprint the downsample check sees for showing `src` in a
/// `target`-sized viewport under `mode`, with square source pixels.
fn footprint(target: UVec2, src: UVec2, mode: ScaleMode) -> UVec2 {
    let (uv_scale, _) = scale_offset(target, src, 0.0, 1.0, mode);
    (target.as_vec2() * uv_scale)
        .round()
        .as_uvec2()
        .max(UVec2::ONE)
}

#[test]
fn minification_is_detected_per_axis() {
    let src = UVec2::new(320, 240);
    // At the default limit, 1:1 and up are not minification — the boundary
    // is exclusive.
    assert!(!wants_downsample(
        footprint(UVec2::new(320, 240), src, ScaleMode::Fit),
        src,
        1.0
    ));
    assert!(!wants_downsample(
        footprint(UVec2::new(1920, 1080), src, ScaleMode::Fit),
        src,
        1.0
    ));
    // A 5x4 grid of a 1920x1080 window still shows it above 1:1 — well
    // under the 1.5x `crt_limit`, but with nothing to filter away.
    assert!(!wants_downsample(
        footprint(UVec2::new(384, 270), src, ScaleMode::Fit),
        src,
        1.0
    ));
    // An 8x6 grid does squeeze it below its source resolution.
    assert!(wants_downsample(
        footprint(UVec2::new(240, 180), src, ScaleMode::Fit),
        src,
        1.0
    ));
    // A half-width Amiga frame stretched to 1:1 vertically is still
    // squeezed horizontally, and aliases there.
    let amiga = UVec2::new(640, 256);
    assert!(wants_downsample(UVec2::new(512, 512), amiga, 1.0));
}

/// The limit is the same kind of threshold as `crt_limit`, from the other
/// side: raising it downsamples views that magnify below it, and `0`
/// switches the downsampler off however small the view gets.
#[test]
fn downsample_limit_thresholds_like_crt_limit() {
    let src = UVec2::new(320, 240);
    // A 5x4 grid cell shows the source at ~1.2x: untouched at the default,
    // downsampled once the limit is raised past that.
    let cell = footprint(UVec2::new(384, 270), src, ScaleMode::Fit);
    assert!(!wants_downsample(cell, src, 1.0));
    assert!(wants_downsample(cell, src, 1.5));
    // Exactly at the limit the effect keeps it — the boundary is exclusive,
    // mirroring `crt_limit`'s inclusive `>=`.
    let one_to_one = footprint(UVec2::new(320, 240), src, ScaleMode::Fit);
    assert!(!wants_downsample(one_to_one, src, 1.0));
    // `0` never downsamples, however squeezed the view is.
    assert!(!wants_downsample(
        footprint(UVec2::new(240, 180), src, ScaleMode::Fit),
        src,
        0.0
    ));
    assert!(!wants_downsample(UVec2::new(1, 1), src, 0.0));
}
