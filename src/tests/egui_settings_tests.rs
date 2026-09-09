use super::*;

use bevy::color::{LinearRgba, Srgba};
use std::path::PathBuf;

#[derive(Reflect, Clone, Copy, Debug, Default, PartialEq)]
enum Mode {
    #[default]
    Stretch,
    Fit,
    Zoom,
}

/// An enum a combo box cannot drive: one of its variants carries a payload.
#[derive(Reflect, Clone, Debug, Default)]
enum Scale {
    #[default]
    Auto,
    Fixed(f32),
}

#[derive(Reflect, Clone, Debug, Default)]
struct Every {
    flag: bool,
    name: String,
    count: u32,
    offset: i8,
    size: usize,
    factor: f32,
    precise: f64,
    tint: Color,
    plain: Srgba,
    linear: LinearRgba,
    mode: Mode,
}

#[derive(Reflect, Clone, Debug, Default)]
struct Unsupported {
    maybe: Option<f32>,
    many: Vec<String>,
    path: PathBuf,
    scale: Scale,
    huge: u128,
}

#[derive(Reflect, Clone, Debug, Default)]
struct Ranged {
    #[reflect(@Range::new(0.0, 240.0))]
    capped: u32,
    #[reflect(@Range::with_speed(-1.0, 1.0, 0.01))]
    tuned: f32,
    free: u32,
}

#[derive(Reflect, Clone, Debug, Default)]
struct WithIgnored {
    kept: bool,
    #[reflect(ignore)]
    #[allow(dead_code)]
    dropped: Vec<String>,
}

#[derive(Reflect, Clone, Debug, Default)]
struct Named {
    idle_timeout: f32,
    aga: bool,
    a: bool,
}

fn widgets(fields: &[Field]) -> Vec<(&str, &Widget)> {
    fields.iter().map(|f| (f.name, &f.widget)).collect()
}

fn widget_of(fields: &[Field], name: &str) -> Widget {
    fields
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no field {name}"))
        .widget
        .clone()
}

#[test]
fn every_supported_type_maps_to_its_widget() {
    let fields = describe(Every::default().as_partial_reflect());
    assert_eq!(
        widgets(&fields),
        vec![
            ("flag", &Widget::Bool),
            ("name", &Widget::Text),
            ("count", &Widget::Int { range: None }),
            ("offset", &Widget::Int { range: None }),
            ("size", &Widget::Int { range: None }),
            ("factor", &Widget::Float { range: None }),
            ("precise", &Widget::Float { range: None }),
            ("tint", &Widget::Color),
            ("plain", &Widget::Color),
            ("linear", &Widget::Color),
            (
                "mode",
                &Widget::Enum {
                    variants: vec!["Stretch", "Fit", "Zoom"],
                },
            ),
        ]
    );
}

/// The regression this ordering exists for: `Color` is itself a reflected enum
/// over its colour spaces, so a dispatcher that checks enums first would offer a
/// combo box of `Srgba`/`Hsla`/... instead of a colour picker.
#[test]
fn colors_are_not_mistaken_for_enums() {
    let fields = describe(Every::default().as_partial_reflect());
    for name in ["tint", "plain", "linear"] {
        assert_eq!(widget_of(&fields, name), Widget::Color, "{name}");
    }
}

#[test]
fn unsupported_types_are_rows_not_panics() {
    let fields = describe(Unsupported::default().as_partial_reflect());
    assert_eq!(
        widgets(&fields),
        vec![
            ("maybe", &Widget::Unsupported),
            ("many", &Widget::Unsupported),
            ("path", &Widget::Unsupported),
            ("scale", &Widget::Unsupported),
            ("huge", &Widget::Unsupported),
        ]
    );
}

#[test]
fn ranges_come_from_field_attributes() {
    let fields = describe(Ranged::default().as_partial_reflect());
    assert_eq!(
        widget_of(&fields, "capped"),
        Widget::Int {
            range: Some(Range::new(0.0, 240.0)),
        }
    );
    assert_eq!(
        widget_of(&fields, "tuned"),
        Widget::Float {
            range: Some(Range::with_speed(-1.0, 1.0, 0.01)),
        }
    );
    assert_eq!(widget_of(&fields, "free"), Widget::Int { range: None });
}

#[test]
fn range_new_picks_a_speed_that_crosses_the_span() {
    let r = Range::new(0.0, 300.0);
    assert_eq!(r.speed, 1.0);
    // Reversed bounds still give a positive speed; egui would otherwise drag the
    // value the wrong way.
    assert!(Range::new(10.0, 0.0).speed > 0.0);
}

/// An integer field gets to state its bounds as integers -- an unsigned literal
/// included, which is what `latency` needs.
#[test]
fn range_takes_integer_bounds() {
    assert_eq!(Range::new(0, 300), Range::new(0.0, 300.0));
    assert_eq!(Range::new(1u32, 8u8), Range::new(1.0, 8.0));
    assert_eq!(
        Range::with_speed(-1, 1, 0.01),
        Range::with_speed(-1.0, 1.0, 0.01)
    );
}

#[test]
fn ignored_fields_do_not_appear() {
    let fields = describe(WithIgnored::default().as_partial_reflect());
    assert_eq!(widgets(&fields), vec![("kept", &Widget::Bool)]);
}

#[test]
fn labels_are_field_names_in_title_case() {
    let fields = describe(Named::default().as_partial_reflect());
    let labels: Vec<&str> = fields.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, vec!["Idle Timeout", "Aga", "A"]);
}

#[test]
fn non_structs_describe_to_nothing() {
    assert!(describe(true.as_partial_reflect()).is_empty());
    assert!(describe(Mode::Fit.as_partial_reflect()).is_empty());
    assert!(describe(vec![1u32, 2].as_partial_reflect()).is_empty());
}

#[test]
fn set_variant_switches_a_unit_enum() {
    let mut mode = Mode::Stretch;
    assert!(set_variant(mode.as_partial_reflect_mut(), "Zoom"));
    assert_eq!(mode, Mode::Zoom);
}

#[test]
fn set_variant_reports_no_change() {
    let mut mode = Mode::Fit;
    // Already there.
    assert!(!set_variant(mode.as_partial_reflect_mut(), "Fit"));
    // No such variant.
    assert!(!set_variant(mode.as_partial_reflect_mut(), "Nonsense"));
    // Not an enum at all.
    assert!(!set_variant(false.as_partial_reflect_mut(), "Fit"));
    assert_eq!(mode, Mode::Fit);
}

#[test]
fn set_variant_reaches_a_field_through_the_struct() {
    let mut every = Every::default();
    let ReflectMut::Struct(s) = every.reflect_mut() else {
        panic!("not a struct");
    };
    let index = describe(Every::default().as_partial_reflect())
        .iter()
        .position(|f| f.name == "mode")
        .unwrap();
    assert!(set_variant(s.field_at_mut(index).unwrap(), "Fit"));
    assert_eq!(every.mode, Mode::Fit);
}
