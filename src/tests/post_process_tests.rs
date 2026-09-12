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

/// The Mega Bezel preset packs lean on three things RetroArch's preset parser
/// does and stock librashader 0.11 does not, so demarc builds against a fork
/// that does them too (see `docs/SHADERS.md`): a `#reference` path ends at its
/// closing quote rather than swallowing a trailing comment, reference depth
/// counts chain levels rather than files visited, and a reference that does not
/// resolve is skipped rather than failing the whole preset. Going back to a
/// librashader without those fixes breaks every preset in those packs, so pin
/// the behaviour here rather than finding out at load time.
#[test]
fn preset_references_follow_retroarch_rules() {
    use librashader::presets::ShaderPreset;
    use std::fmt::Write as _;

    let dir = std::env::temp_dir().join("demarc-preset-reference-rules");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");

    let stock = crate::system_dir().join("shaders/slangp/stock.slangp");
    let mut root = format!("#reference \"{}\" // the passes\n", stock.display());
    // Well past SHADER_MAX_REFERENCE_DEPTH (16) files, but only one level deep:
    // a pack preset pulls in this many .params siblings, each one annotated.
    for i in 0..20 {
        let leaf = dir.join(format!("leaf{i}.params"));
        std::fs::write(&leaf, format!("leaf_param{i} = \"{i}.0\"\n")).expect("leaf");
        let _ = writeln!(root, "#reference \"leaf{i}.params\" // leaf {i}");
    }
    // A reference to a file the pack never shipped: RetroArch warns and moves on.
    root.push_str("#reference \"absent.params\" // renamed upstream\n");

    let root_path = dir.join("root.slangp");
    std::fs::write(&root_path, root).expect("root preset");

    let preset = ShaderPreset::try_parse(&root_path, ShaderFeatures::NONE)
        .unwrap_or_else(|err| panic!("annotated reference chain should parse: {err}"));
    assert_eq!(preset.passes.len(), 1, "should inherit the stock pass");
    assert!(
        preset.passes[0].path.is_file(),
        "reference paths should not keep their quotes: {:?}",
        preset.passes[0].path
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Parse a slice of an installed Mega Bezel pack. Ignored because it needs the
/// pack and a `slang-shaders` checkout laid out as `docs/SHADERS.md` describes;
/// run it after updating either, or after `scripts/fix-megabezel-pack.sh`.
#[test]
#[ignore]
fn megabezel_pack_presets_resolve() {
    use librashader::presets::ShaderPreset;

    let root = PathBuf::from("shaders/Mega_Bezel_Packs/TheNamec-Commodore/presets");
    assert!(root.is_dir(), "no pack at {root:?}");

    let mut presets = Vec::new();
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).expect("readable").flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "slangp") {
                presets.push(path);
            }
        }
    }
    presets.sort();
    assert!(!presets.is_empty(), "no presets found");

    // The pack ships ~72k presets built from a few hundred building blocks;
    // every 200th covers each device/flavour/scene combination many times over.
    let mut checked = 0;
    for path in presets.iter().step_by(200) {
        let preset = ShaderPreset::try_parse(path, ShaderFeatures::NONE)
            .unwrap_or_else(|err| panic!("{path:?}: {err}"));
        // No passes means every reference that carried them went missing —
        // the pack and the Mega Bezel checkout have drifted apart.
        assert!(!preset.passes.is_empty(), "{path:?} resolved to no passes");
        checked += 1;
    }
    println!("{checked} of {} presets parsed", presets.len());
}

/// A shader change starts one build and, until it lands, exactly one: the
/// selections the user clicked past on the way are never compiled.
#[test]
fn rapid_shader_changes_coalesce_onto_one_build() {
    let a = Path::new("/presets/a.slangp");
    let b = Path::new("/presets/b.slangp");
    let c = Path::new("/presets/c.slangp");

    // Nothing ready, nothing running: the first draw starts the build.
    assert!(should_start(None, None, a, false));
    // Already building what we want, or already running it: no second build.
    assert!(!should_start(None, Some(a), a, false));
    assert!(!should_start(Some(a), None, a, false));
    // The dialog moves on twice while a's build still holds a pool thread.
    // Neither b nor c is started — b is skipped entirely.
    assert!(!should_start(None, Some(a), b, false));
    assert!(!should_start(None, Some(a), c, false));
    // a lands and is dropped (it is not what is selected any more); only now,
    // and only for the current selection, does a build start.
    assert!(should_start(None, None, c, false));
    // The same holds with an older preset still on screen: the switch away from
    // a chain that is already rendering is what a shader change is.
    assert!(should_start(Some(a), None, c, false));
}

/// A preset that failed to load is not retried on every frame — and is not
/// papered over with the preset the user switched away from.
#[test]
fn a_failed_preset_is_not_retried() {
    let a = Path::new("/presets/a.slangp");
    let broken = Path::new("/presets/broken.slangp");
    assert!(!should_start(Some(a), None, broken, true));
    assert!(!should_start(None, None, broken, true));
}

/// A 4:3 wine release in a 1280x1024 gamescope session: the frame is the whole
/// session, with the picture scaled into the middle of it.
const LETTERBOX: (UVec2, UVec2) = (UVec2::new(1280, 1024), UVec2::new(1280, 960));

/// The screen-uv range the shader samples from the source, per axis.
fn sampled(target: UVec2, mode: ScaleMode) -> (Vec2, Vec2) {
    let (src, used) = LETTERBOX;
    let (scale, offset) = view_transform(target, src, used, 1.25, 1.0, mode);
    (-offset / scale, (Vec2::ONE - offset) / scale)
}

/// Whatever the mode, the border is never on screen: Stretch and Fit sample the
/// picture exactly, and Zoom crops into it.
#[test]
fn the_border_is_cropped_away() {
    const TOP: f32 = 0.031_25;
    const BOTTOM: f32 = 0.968_75;

    for mode in [ScaleMode::Stretch, ScaleMode::Fit] {
        let (min, max) = sampled(UVec2::new(1920, 1080), mode);
        assert!((min.y - TOP).abs() < 1e-4, "{mode:?} top {}", min.y);
        assert!((max.y - BOTTOM).abs() < 1e-4, "{mode:?} bottom {}", max.y);
    }

    // 4:3 zoomed to fill 16:9 throws away the top and bottom of the picture —
    // of the picture, not of the frame.
    let (min, max) = sampled(UVec2::new(1920, 1080), ScaleMode::Zoom);
    assert!(min.y > TOP && max.y < BOTTOM, "{min:?} {max:?}");
}

/// Fit works on the picture's 4:3, not on the frame's 5:4: in a 16:9 window the
/// picture is 4/3 / (16/9) of the width and fills the height.
#[test]
fn fit_uses_the_picture_aspect() {
    let (src, used) = LETTERBOX;
    let (scale, offset) =
        view_transform(UVec2::new(1920, 1080), src, used, 1.25, 1.0, ScaleMode::Fit);
    let f = used_fraction(src, used);
    // Where the picture itself lands, the way `compute_uniform` takes it back.
    let size = scale * f;
    let corner = offset + scale * (Vec2::ONE - f) * 0.5;
    assert!((size.x - 0.75).abs() < 1e-4, "{size:?}");
    assert!((size.y - 1.0).abs() < 1e-4, "{size:?}");
    // Centred, so the pillarbox bars are equal.
    assert!((corner.x - 0.125).abs() < 1e-4, "{corner:?}");
}

/// A source with no border is left exactly as `scale_offset` had it.
#[test]
fn no_border_is_the_plain_transform() {
    let target = UVec2::new(1920, 1080);
    let src = UVec2::new(320, 240);
    for used in [UVec2::ZERO, src] {
        for mode in [ScaleMode::Fit, ScaleMode::Zoom, ScaleMode::Fixed(2.0)] {
            assert_eq!(
                view_transform(target, src, used, 0.0, 1.0, mode),
                scale_offset(target, src, 0.0, 1.0, mode),
                "{used:?} {mode:?}"
            );
        }
    }
}
