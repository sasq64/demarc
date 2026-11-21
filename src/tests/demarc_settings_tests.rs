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
    assert_eq!(titles, vec!["", "Dos", "Amiga", "Recording", "Wine"]);
    let names: Vec<&str> = widgets(&sections[4].fields)
        .iter()
        .map(|(n, _)| *n)
        .collect();
    assert_eq!(names, vec!["resolution", "show_startup_dialog"]);
}

#[test]
fn resolutions_are_labelled_by_size() {
    let sections = settings_sections();
    let Widget::Enum { variants } = &sections[4].fields[0].widget else {
        panic!("resolution is not a combo box");
    };
    let labels: Vec<&str> = variants.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "Auto",
            "320x200",
            "512x384",
            "640x480",
            "800x600",
            "1024x768",
            "1280x1024",
            "800x450",
            "1024x576",
            "1280x720",
            "1920x1080",
            "2560x1440"
        ]
    );
}

#[test]
fn latency_is_picked_from_its_levels() {
    let sections = settings_sections();
    let latency = sections[0].fields.iter().find(|f| f.name == "latency");
    let Some(Widget::Enum { variants }) = latency.map(|f| &f.widget) else {
        panic!("latency is not a combo box");
    };
    let labels: Vec<&str> = variants.iter().map(|v| v.label.as_str()).collect();
    assert_eq!(labels, vec!["VeryLow", "Low", "Medium", "High"]);
}

#[test]
fn wine_settings_write_and_clear_meta() {
    let mut meta = HashMap::from([(META_RES.to_owned(), "640x480".to_owned())]);
    let old = WineSettings::default();
    let new = WineSettings {
        resolution: Resolution::Res1024x768,
        show_startup_dialog: true,
        ..default()
    };
    apply_wine(&new, &old, &mut meta);
    assert_eq!(meta[META_RES], "1024x768");
    assert_eq!(meta[META_DIALOG_RES], PICK);

    apply_wine(&old, &new, &mut meta);
    assert!(meta.is_empty(), "{meta:?}");
}

#[test]
fn dos_settings_map_to_core_options() {
    assert_eq!(CpuCycles::Auto.as_meta(), "auto");
    assert_eq!(CpuCycles::X486.as_meta(), "25000");
    assert_eq!(CpuCycles::Max.as_meta(), "max");
    assert_eq!(CpuType::MMx.as_meta(), "pentium_mmx");
}

#[test]
fn amiga_settings_map_to_run_wide_meta() {
    let old = AmigaSettings::default();
    let new = AmigaSettings {
        machine: Machine::A4000,
        chip_mem: ChipMemSize::Mb2,
        fast_mem: FastMemSize::Mb32,
        drive_sound: false,
        ..Default::default()
    };
    let mut meta = HashMap::new();
    apply_amiga(&new, &old, &mut meta);
    assert_eq!(meta["amiberry_model"], "A4040");
    assert_eq!(meta["amiberry_chipmem_size"], "4");
    assert_eq!(meta["amiberry_fastmem_size"], "0");
    assert_eq!(meta["amiberry_z3mem_size"], "32");
    assert_eq!(meta["amiberry_floppy_sound"], "disabled");
    assert!(!meta.contains_key("amiberry_bogomem_size"));

    apply_amiga(&old, &new, &mut meta);
    assert_eq!(meta.remove("amiberry_floppy_sound").as_deref(), Some("enabled"));
    assert!(meta.is_empty(), "{meta:?}");
}
