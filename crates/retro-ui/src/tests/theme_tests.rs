use super::*;

/// Apart from the text colour, the default theme is egui's own dark look.
#[test]
fn default_theme_is_egui_dark() {
    let mut style = egui::Style::default();
    Theme::DEFAULT.apply(&mut style);
    let mut want = egui::Style::default();
    want.visuals.override_text_color = Some(Theme::DEFAULT.text);
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
    t.text = Color32::RED;
    set_theme(&ctx, t.clone());
    assert_eq!(*theme(&ctx), t);
    assert_eq!(ctx.global_style().visuals.override_text_color, Some(Color32::RED));
}
