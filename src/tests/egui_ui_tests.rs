use super::*;

/// Lays the picker's list box out on a `size`-sized screen for `frames` frames
/// and returns the height of the box on each of them. The list lives in an
/// anchored [`egui::Area`] with a panel above it, as [`render_list`] draws it.
fn list_heights(size: egui::Vec2, items: usize, frames: usize) -> Vec<f32> {
    let ctx = egui::Context::default();
    let ids: Vec<usize> = (0..items).collect();
    let mut heights = Vec::new();
    for _ in 0..frames {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };
        let mut height = 0.0;
        ctx.begin_pass(input);
        {
            let ctx = &ctx;
            egui::Area::new(egui::Id::new("fuzzy_list"))
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_width(size.y);
                    panel_frame().show(ui, |ui| {
                        ui.label(egui::RichText::new("query").size(QUERY_SIZE));
                    });
                    let out = scroll_area(ui, items.saturating_sub(1), 0.0, &ids, |_, _, _| {});
                    height = out.inner_rect.height();
                });
        }
        // Nothing uploads the font texture here, and dropping the delta
        // unapplied is an assertion failure in debug builds.
        ctx.end_pass().textures_delta.clear();
        heights.push(height);
    }
    heights
}

/// The box is its full height from the very first frame, however little the
/// rest of the picker asks for: the area's stale size must not cap it.
#[test]
fn list_box_is_full_height_at_once() {
    let size = egui::vec2(1920.0, 1080.0);
    let want = (size.y * LIST_HEIGHT_FRACTION / ROW_HEIGHT).floor() * ROW_HEIGHT;
    for h in list_heights(size, 200, 3) {
        assert!((h - want).abs() < 1.0, "list box {h} points tall, want {want}");
    }
}
