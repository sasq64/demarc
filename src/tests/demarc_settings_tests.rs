use super::*;

use crate::egui_settings::{Field, Range, Widget, describe};

fn widgets(fields: &[Field]) -> Vec<(&str, &Widget)> {
    fields.iter().map(|f| (f.name, &f.widget)).collect()
}

/// The app's own settings are what the hotkey opens, so every field has to
/// reach a widget it can actually be edited with -- a row that came out
/// `Unsupported` is a setting nobody can change.
#[test]
fn demarc_settings_are_all_editable() {
    let fields = describe(DemarcSettings::default().as_partial_reflect());
    assert_eq!(
        widgets(&fields),
        vec![
            ("fullscreen", &Widget::Bool),
            ("background", &Widget::Color),
            ("fast_load", &Widget::Bool),
            (
                "resolution",
                &Widget::Enum {
                    variants: vec![
                        "Res640x480",
                        "Res800x600",
                        "Res1024x768",
                        "Res1280x720",
                        "Res1920x1080",
                    ],
                },
            ),
            (
                "shader",
                &Widget::Enum {
                    variants: vec!["Lottes", "LottesSimple", "Lcd", "LcdSimple", "None"],
                },
            ),
            (
                "latency",
                &Widget::Int {
                    range: Some(Range::new(1, 8)),
                },
            ),
            (
                "volume",
                &Widget::Float {
                    range: Some(Range::new(0.0, 100.0)),
                },
            ),
        ]
    );
}

/// Every variant the combo box offers has to resolve to a shader that exists,
/// or picking it would leave a black screen. The `.wgsl` ones are asset paths
/// (relative to the `system` dir, which is the Bevy asset root); the presets
/// are absolute.
#[test]
fn every_shader_variant_resolves_to_a_file() {
    use crate::config::ShaderArg;
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
/// which is only correct if two different variants really are different values.
#[test]
fn shader_variants_compare_unequal() {
    use crate::config::ShaderArg;
    assert_ne!(ShaderArg::Lottes, ShaderArg::LottesSimple);
    assert_eq!(ShaderArg::Lcd, ShaderArg::Lcd);
}
