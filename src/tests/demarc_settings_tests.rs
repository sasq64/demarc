use super::*;

use bevy::reflect::TypeRegistry;

use crate::egui_settings::{Field, Widget, sections};

fn widgets(fields: &[Field]) -> Vec<(&str, &Widget)> {
    fields.iter().map(|f| (f.name, &f.widget)).collect()
}

fn settings_sections() -> Vec<crate::egui_settings::Section> {
    let mut registry = TypeRegistry::new();
    registry.register::<DemarcSettings>();
    sections(DemarcSettings::default().as_partial_reflect(), &registry)
}

/// The app's own settings are what the hotkey opens, so every field has to
/// reach a widget it can actually be edited with -- a row that came out
/// `Unsupported` is a setting nobody can change.
#[test]
fn demarc_settings_are_all_editable() {
    for section in settings_sections() {
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

#[test]
fn wine_is_its_own_section() {
    let sections = settings_sections();
    let titles: Vec<&str> = sections.iter().map(|s| s.title.as_str()).collect();
    assert_eq!(titles, vec!["", "Wine"]);
    let names: Vec<&str> = widgets(&sections[1].fields)
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(
        names,
        vec!["resolution", "overrides", "show_startup_dialog", "filter"]
    );
}

#[test]
fn resolutions_are_labelled_by_size() {
    let sections = settings_sections();
    let Widget::Enum { variants } = &sections[1].fields[0].widget else {
        panic!("resolution is not a combo box");
    };
    let labels: Vec<&str> = variants.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Auto",
            "640x480",
            "800x600",
            "1024x768",
            "1280x720",
            "1920x1080"
        ]
    );
}

#[test]
fn wine_settings_write_and_clear_meta() {
    let mut meta = HashMap::from([(META_RES.to_owned(), "640x480".to_owned())]);
    let old = WineSettings::default();
    let new = WineSettings {
        resolution: Resolution::Res1024x768,
        overrides: "d3d9=n".into(),
        show_startup_dialog: true,
        ..default()
    };
    apply_wine(&new, &old, &mut meta);
    assert_eq!(meta[META_RES], "1024x768");
    assert_eq!(meta[META_DLL_OVERRIDES], "d3d9=n");
    assert_eq!(meta[META_DIALOG_RES], PICK);

    apply_wine(&old, &new, &mut meta);
    assert!(meta.is_empty(), "{meta:?}");
}
