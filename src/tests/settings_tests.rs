use super::*;

use bevy::color::{LinearRgba, Srgba};
use std::path::PathBuf;

/// A registry holding `T` and everything it is built out of -- what the dialog
/// gets from `AppTypeRegistry` at runtime, since `add_settings_type` registers
/// the settings struct.
fn registry<T: GetTypeRegistration>() -> TypeRegistry {
    let mut registry = TypeRegistry::new();
    registry.register::<T>();
    registry
}

/// `describe` over a default-constructed `T`, which is how all but a couple of
/// these tests call it.
fn fields_of<T: Reflect + GetTypeRegistration + Default>() -> Vec<Field> {
    describe(T::default().as_partial_reflect(), &registry::<T>())
}

/// The variant list of a combo box field, as (name, label) pairs.
fn variants_of(fields: &[Field], name: &str) -> Vec<(&'static str, String)> {
    match widget_of(fields, name) {
        Widget::Enum { variants } => variants.into_iter().map(|v| (v.name, v.label)).collect(),
        other => panic!("{name} is {other:?}, not a combo box"),
    }
}

/// A combo box's entries where the enum has no `Display` impl: label == name.
fn plain(names: &[&'static str]) -> Vec<Variant> {
    names
        .iter()
        .map(|&name| Variant {
            name,
            label: name.to_owned(),
        })
        .collect()
}

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
    cross_fade_delay: f32,
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
    let fields = fields_of::<Every>();
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
                    variants: plain(&["Stretch", "Fit", "Zoom"]),
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
    let fields = fields_of::<Every>();
    for name in ["tint", "plain", "linear"] {
        assert_eq!(widget_of(&fields, name), Widget::Color, "{name}");
    }
}

#[test]
fn unsupported_types_are_rows_not_panics() {
    let fields = fields_of::<Unsupported>();
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
    let fields = fields_of::<Ranged>();
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
    let fields = fields_of::<WithIgnored>();
    assert_eq!(widgets(&fields), vec![("kept", &Widget::Bool)]);
}

#[test]
fn labels_are_field_names_in_title_case() {
    let fields = fields_of::<Named>();
    let labels: Vec<&str> = fields.iter().map(|f| f.label.as_str()).collect();
    assert_eq!(labels, vec!["Cross Fade Delay", "Aga", "A"]);
}

#[test]
fn non_structs_describe_to_nothing() {
    let registry = registry::<Mode>();
    assert!(describe(true.as_partial_reflect(), &registry).is_empty());
    assert!(describe(Mode::Fit.as_partial_reflect(), &registry).is_empty());
    assert!(describe(vec![1u32, 2].as_partial_reflect(), &registry).is_empty());
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
    let index = fields_of::<Every>()
        .iter()
        .position(|f| f.name == "mode")
        .unwrap();
    assert!(set_variant(s.field_at_mut(index).unwrap(), "Fit"));
    assert_eq!(every.mode, Mode::Fit);
}

/// The app's own settings are what the hotkey opens, so every field has to
/// reach a widget it can actually be edited with -- a row that came out
/// `Unsupported` is a setting nobody can change.
#[test]
fn demo_settings_are_all_editable() {
    let registry = registry::<DemarcSettings>();
    for section in sections(DemarcSettings::default().as_partial_reflect(), &registry) {
        for field in &section.fields {
            assert_ne!(
                field.widget,
                Widget::Unsupported,
                "{}.{}",
                section.title,
                field.name
            );
        }
    }
}

/// The nested `wine: WineSettings` is a heading with its own rows, not a row.
#[test]
fn nested_structs_become_sections() {
    let registry = registry::<DemarcSettings>();
    let sections = sections(DemarcSettings::default().as_partial_reflect(), &registry);
    let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["", "Wine"]);

    let root = &sections[0];
    assert!(root.path.is_empty());
    assert_eq!(widget_of(&root.fields, "fullscreen"), Widget::Bool);
    assert_eq!(widget_of(&root.fields, "wine"), Widget::Section);

    let wine = &sections[1];
    // The path is the index of `wine` in the root struct, which is what
    // `field_at_path` walks to reach the value the rows edit.
    assert_eq!(
        wine.path,
        vec![
            root.fields
                .iter()
                .position(|f| f.name == "wine")
                .expect("no wine field")
        ]
    );
    let names: Vec<&str> = wine.fields.iter().map(|f| f.name).collect();
    assert_eq!(
        names,
        vec!["resolution", "overrides", "show_startup_dialog", "filter"]
    );
    assert_eq!(widget_of(&wine.fields, "overrides"), Widget::Text);
    assert_eq!(widget_of(&wine.fields, "show_startup_dialog"), Widget::Bool);
    assert_eq!(widget_of(&wine.fields, "filter"), Widget::Bool);
}

/// `Resolution`'s variants cannot be named `640x480`, so the combo box labels
/// them with the enum's own `Display` impl -- the names stay what `set_variant`
/// needs.
#[test]
fn display_impl_labels_an_enums_variants() {
    let registry = registry::<DemarcSettings>();
    let sections = sections(DemarcSettings::default().as_partial_reflect(), &registry);
    let wine = sections
        .iter()
        .find(|s| s.title == "Wine")
        .expect("no wine");
    assert_eq!(
        variants_of(&wine.fields, "resolution"),
        vec![
            ("Auto", "Auto".to_owned()),
            ("Res640x480", "640x480".to_owned()),
            ("Res800x600", "800x600".to_owned()),
            ("Res1024x768", "1024x768".to_owned()),
            ("Res1280x720", "1280x720".to_owned()),
            ("Res1920x1080", "1920x1080".to_owned()),
        ]
    );
}

/// The two wine fields that share the `wine_res` key: asking for the demo's own
/// setup dialog wins over a size, and `Auto` is the empty string that
/// [`crate::newsys::GlobalMeta::set_or_clear`] reads as "take the key away" --
/// which is what leaves a release free to be run at the size its file name asks
/// for.
#[test]
fn the_wine_resolution_and_the_startup_dialog_share_one_key() {
    let wine = |resolution, show_startup_dialog| {
        WineSettings {
            resolution,
            show_startup_dialog,
            ..default()
        }
        .wine_res()
    };
    assert_eq!(wine(Resolution::Auto, false), "");
    assert_eq!(wine(Resolution::Res1024x768, false), "1024x768");
    assert_eq!(wine(Resolution::Res1024x768, true), "pick");
    assert_eq!(wine(Resolution::Auto, true), "pick");
}

/// An enum without `#[reflect(Display)]` keeps its variant identifiers -- and so
/// does one whose type never made it into the registry.
#[test]
fn variants_without_display_keep_their_names() {
    assert_eq!(
        variants_of(&fields_of::<Every>(), "mode"),
        vec![
            ("Stretch", "Stretch".to_owned()),
            ("Fit", "Fit".to_owned()),
            ("Zoom", "Zoom".to_owned()),
        ]
    );
    let empty = TypeRegistry::empty();
    let fields = describe(WineSettings::default().as_partial_reflect(), &empty);
    // Not `Auto`, whose `Display` output and identifier are the same word and so
    // would pass either way.
    assert_eq!(
        variants_of(&fields, "resolution")[1],
        ("Res640x480", "Res640x480".to_owned())
    );
}

/// Sections are depth first, and a section deeper than one carries the labels
/// of the fields it came through.
#[test]
fn sections_nest_and_name_their_path() {
    #[derive(Reflect, Clone, Debug, Default)]
    struct Inner {
        deep: bool,
    }
    #[derive(Reflect, Clone, Debug, Default)]
    struct Middle {
        mid: u32,
        inner: Inner,
    }
    #[derive(Reflect, Clone, Debug, Default)]
    struct Outer {
        top: bool,
        first: Middle,
        tint: Color,
    }

    let sections = sections(Outer::default().as_partial_reflect(), &registry::<Outer>());
    let found: Vec<(&str, &[usize])> = sections
        .iter()
        .map(|s| (s.title.as_str(), s.path.as_slice()))
        .collect();
    // `tint` is a struct under the hood and must stay a colour row.
    assert_eq!(
        found,
        vec![
            ("", &[][..]),
            ("First", &[1][..]),
            ("First / Inner", &[1, 1][..]),
        ]
    );

    let mut outer = Outer::default();
    let deep = field_at_path(outer.as_partial_reflect_mut(), &[1, 1]).expect("no such path");
    let ReflectMut::Struct(deep) = deep.reflect_mut() else {
        panic!("not a struct");
    };
    *deep
        .field_mut("deep")
        .unwrap()
        .try_downcast_mut::<bool>()
        .unwrap() = true;
    assert!(outer.first.inner.deep);
    // A path through something that is not a struct resolves to nothing.
    assert!(field_at_path(outer.as_partial_reflect_mut(), &[0, 0]).is_none());
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
