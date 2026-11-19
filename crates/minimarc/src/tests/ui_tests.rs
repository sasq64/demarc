use super::*;

#[test]
fn the_heart_shows_left_of_the_info_text() {
    let mut ui = Ui::new();
    let mut files = vec![];
    database::files::collect_db_text(
        "id:1\ttitle:A\tauthor:G\tcategory:Demo\tdownload:http://x/a.zip\n",
        &Default::default(),
        &mut files,
    );
    ui.playing = files.pop();
    ui.info("\"A\"\nG (2025)\nC64 Demo");
    ui.keys_open = true;
    let ctx = ui.ctx.clone();
    for _ in 0..20 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(538.0, 600.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |egui_ui| {
            ui.draw(egui_ui.ctx());
        });
        out.textures_delta.clear();
    }
    let heart = ui.key_rects.iter().find(|(b, _)| *b == Button::Favorite);
    let (_, rect) = heart.expect("no heart");
    assert!(rect.max.x < 538.0 && rect.min.y >= 0.0);
}

#[test]
fn the_heart_is_not_part_of_the_key_bar() {
    let mut ui = Ui::new();
    ui.keys_open = true;
    let ctx = ui.ctx.clone();
    let mut draw = |ui: &mut Ui| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(538.0, 600.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |egui_ui| {
            ui.draw(egui_ui.ctx());
        });
        out.textures_delta.clear();
    };
    for _ in 0..3 {
        draw(&mut ui);
    }
    let bar = ui.key_bar_height();
    let mut files = vec![];
    database::files::collect_db_text(
        "id:1\ttitle:A\tdownload:http://x/a.zip\n",
        &Default::default(),
        &mut files,
    );
    ui.playing = files.pop();
    ui.info("A");
    for _ in 0..20 {
        draw(&mut ui);
    }
    assert!(ui.key_rects.iter().any(|(b, _)| *b == Button::Favorite));
    assert_eq!(ui.key_bar_height(), bar);
}
