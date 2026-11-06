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
    assert_eq!(
        ctx.global_style().visuals.override_text_color,
        Some(Color32::RED)
    );
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
    keys(
        &toml::Table::try_from(Theme::DEFAULT).unwrap(),
        "",
        &mut want,
    );
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

#[test]
fn panel_glow_becomes_the_frame_shadow() {
    let theme = Theme::from_toml(
        r##"
        [palette]
        peach = "#ffaa7c"
        [panel]
        glow = { blur = 24, spread = 4, color = "peach" }
        "##,
    )
    .unwrap();
    let shadow = theme.panel.frame().shadow;
    assert_eq!(shadow.offset, [0, 0]);
    assert_eq!((shadow.blur, shadow.spread), (24, 4));
    assert_eq!(shadow.color, Color32::from_rgb(0xff, 0xaa, 0x7c));
}

#[test]
fn shader_paths_are_not_palette_names() {
    let theme = Theme::from_toml(
        r##"
        [palette]
        stars = "#ffffff"
        [panel.shader]
        shader = "stars"
        texture = "stars.png"
        [screen.shader]
        shader = "plasma.wgsl"
        "##,
    )
    .unwrap();
    let panel = theme.panel.shader.unwrap();
    assert_eq!((panel.shader.as_str(), panel.texture.as_deref()), ("stars", Some("stars.png")));
    assert_eq!(theme.screen.shader.unwrap().shader, "plasma.wgsl");
}

#[test]
fn shaders_are_painted_under_the_panel_and_over_the_screen() {
    use std::sync::Mutex;
    let painted = Arc::new(Mutex::new(Vec::new()));
    let mut harness = crate::tests::Harness::new();
    let log = painted.clone();
    crate::set_shader_painter(
        &harness.ctx,
        Arc::new(move |rect, bg: &ShaderBg| {
            log.lock().unwrap().push((rect, bg.shader.clone()));
            Some(egui::Shape::rect_filled(rect, 0, Color32::RED))
        }),
    );
    let mut theme = Theme::DEFAULT;
    theme.panel.shader = Some(ShaderBg { shader: "panel.wgsl".into(), texture: None });
    theme.screen.shader = Some(ShaderBg { shader: "screen.wgsl".into(), texture: None });
    set_theme(&harness.ctx, theme);
    let mut panel = egui::Rect::NOTHING;
    harness.frame(vec![], |ctx| {
        paint_screen(ctx);
        egui::Area::new(egui::Id::new("p")).show(ctx, |ui| {
            panel = crate::show_panel(ui, |ui| ui.label("x")).response.rect;
        });
    });
    let painted = painted.lock().unwrap();
    assert_eq!(painted[0], (harness.ctx.content_rect(), "screen.wgsl".into()));
    assert_eq!(painted[1], (panel, "panel.wgsl".into()));
}

#[test]
fn font_paths_are_not_palette_names() {
    let theme = Theme::from_toml(
        r##"
        [palette]
        topaz = "#ffffff"
        [picker]
        font = "topaz"
        "##,
    )
    .unwrap();
    assert_eq!(theme.fonts(), [None, Some("topaz"), None]);
}

#[test]
fn icon_color_mixes_by_its_alpha() {
    let blue = Color32::from_rgba_unmultiplied(0, 0, 255, 200);
    let mut picker = Theme::DEFAULT.picker;
    assert_eq!(picker.tint_icon(blue), blue);
    picker.icon_color = Color32::WHITE;
    assert_eq!(picker.tint_icon(blue).to_srgba_unmultiplied(), [255, 255, 255, 200]);
    picker.icon_color = Color32::from_rgba_unmultiplied(0, 255, 0, 128);
    assert_eq!(picker.tint_icon(blue).to_srgba_unmultiplied(), [0, 128, 128, 200]);
}
