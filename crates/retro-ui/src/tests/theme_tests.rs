use super::*;

/// Apart from the text colour, the default theme is egui's own dark look.
#[test]
fn default_theme_is_egui_dark() {
    let mut style = egui::Style::default();
    Theme::DEFAULT.apply(&mut style);
    let mut want = egui::Style::default();
    want.visuals.override_text_color = Some(Theme::DEFAULT.text.color);
    assert_eq!(style.visuals, want.visuals);
    assert_eq!(style.spacing.menu_margin, want.spacing.menu_margin);
}

#[test]
fn theme_falls_back_to_default() {
    assert_eq!(*theme(&egui::Context::default()), Theme::DEFAULT);
}

#[test]
fn set_theme_is_read_back() {
    let ctx = egui::Context::default();
    let mut t = Theme::DEFAULT;
    t.text.color = Color32::RED;
    set_theme(&ctx, t.clone());
    assert_eq!(*theme(&ctx), t);
    assert_eq!(ctx.global_style().visuals.override_text_color, Some(Color32::RED));
}

fn default_file() -> String {
    std::fs::read_to_string(concat!(env!("DEMARC_ROOT"), "/system/themes/default.toml")).unwrap()
}

fn keys(table: &toml::Table, prefix: &str, out: &mut Vec<String>) {
    for (key, value) in table {
        let path = format!("{prefix}{key}");
        match value {
            toml::Value::Table(t) if !t.contains_key("width") => keys(t, &format!("{path}."), out),
            _ => out.push(path),
        }
    }
}

/// `default.toml` is the documented template, so it must spell out every key
/// with the default's value.
#[test]
fn default_file_is_the_whole_default_theme() {
    let file: toml::Table = default_file().parse().unwrap();
    let (mut have, mut want) = (Vec::new(), Vec::new());
    keys(&file, "", &mut have);
    keys(&toml::Table::try_from(Theme::DEFAULT).unwrap(), "", &mut want);
    have.retain(|k| !k.starts_with("palette."));
    have.sort();
    want.sort();
    assert_eq!(have, want);
    assert_eq!(Theme::from_toml(&default_file()).unwrap(), Theme::DEFAULT);
}

#[test]
fn a_theme_overrides_only_what_it_names() {
    let theme = Theme::from_toml(
        r##"
        [palette]
        green = "#33ff33"
        [text]
        color = "green"
        [panel]
        border = { width = 3.0, color = "#ff000080" }
        "##,
    )
    .unwrap();
    let mut want = Theme::DEFAULT;
    want.text.color = Color32::from_rgb(0x33, 0xff, 0x33);
    want.panel.border = Stroke::new(3.0, Color32::from_rgba_unmultiplied(0xff, 0, 0, 0x80));
    assert_eq!(theme, want);
}

#[test]
fn theme_errors_are_reported() {
    for src in [
        "[text]\ncolor = \"nope\"",
        "[text]\ncolor = \"#12345\"",
        "txt = \"#ffffff\"",
        "[panel]\nborder = 2.0",
        "[text]\ncolor = ",
    ] {
        assert!(Theme::from_toml(src).is_err(), "{src}");
    }
}
