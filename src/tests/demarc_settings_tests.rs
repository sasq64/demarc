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
