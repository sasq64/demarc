use super::*;

/// Every `--shader` variant has to resolve to a shader that exists, or picking
/// it would leave a black screen. The `.wgsl` ones are asset paths (relative to
/// the `system` dir, which is the Bevy asset root); the presets are absolute.
#[test]
fn every_shader_variant_resolves_to_a_file() {
    use crate::post_process::ShaderEffect;
    use clap::ValueEnum;

    for shader in ShaderArg::value_variants() {
        let path = match shader.effect() {
            ShaderEffect::Slangp(path) => path,
            ShaderEffect::Wgsl(asset) => crate::system_dir::system_dir().join(asset),
        };
        assert!(path.is_file(), "{shader:?} -> {path:?}");
    }
}

/// Applying a shader means re-pointing the render world at a different preset,
/// which is only correct if two different variants really are different values
/// (`crate::shader_dialog` compares them).
#[test]
fn shader_variants_compare_unequal() {
    assert_ne!(ShaderArg::Lottes, ShaderArg::LottesSimple);
    assert_eq!(ShaderArg::Lcd, ShaderArg::Lcd);
}
