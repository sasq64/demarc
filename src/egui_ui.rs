use bevy::{prelude::*, window::PrimaryWindow};
use bevy_egui::{EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, egui};
use std::{collections::HashMap, ops::Range, sync::Arc, time::Duration};

/// Key the app font is registered under in [`egui::FontDefinitions::font_data`].
const APP_FONT: &str = "app";

pub struct EguiUiPlugin;

/// Keeps `font.ttf` alive for [`setup_egui`]. Loading through the asset server
/// rather than reading [`crate::frontend::system_dir`] directly means egui picks
/// up the very same face -- and the same hot-reloaded bytes -- as the Bevy UI in
/// [`crate::hud`] and [`crate::text_input`].
#[derive(Resource)]
struct AppFont(Handle<Font>);

fn load_font(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(AppFont(asset_server.load("font.ttf")));
}

const HEADING_SIZE: f32 = 60.0;
const BODY_SIZE: f32 = 44.0;
const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xff, 0xff);
const MARGIN: egui::Vec2 = egui::vec2(32.0, 32.0);

fn setup_egui(
    mut contexts: EguiContexts,
    app_font: Res<AppFont>,
    fonts: Res<Assets<Font>>,
    mut done: Local<bool>,
) -> Result {
    if *done {
        return Ok(());
    }
    let Some(font) = fonts.get(&app_font.0) else {
        return Ok(());
    };
    let ctx = contexts.ctx_mut()?;

    // egui owns its font bytes (it re-parses them for its own atlas), so this
    // copies out of the Bevy asset instead of sharing the `Blob`.
    let mut font_defs = egui::FontDefinitions::default();
    font_defs.font_data.insert(
        APP_FONT.to_owned(),
        Arc::new(egui::FontData::from_owned(font.data.data().to_vec())),
    );
    // Front of the list = primary; egui's own fonts stay behind it as fallbacks
    // for glyphs `font.ttf` happens to be missing.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        font_defs
            .families
            .entry(family)
            .or_default()
            .insert(0, APP_FONT.to_owned());
    }
    ctx.set_fonts(font_defs);

    ctx.all_styles_mut(|style| {
        style.text_styles.insert(
            egui::TextStyle::Heading,
            egui::FontId::proportional(HEADING_SIZE),
        );
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(BODY_SIZE));
        style.visuals.override_text_color = Some(TEXT_COLOR);
    });

    *done = true;
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq, Hash, Clone, Copy)]
pub enum HudLocation {
    #[default]
    InfoText,
    BottomLeft,
    TopLeft,
    TopRight,
    Error,
}

#[derive(Default, Message, Clone)]
pub struct SetHudText {
    pub text: String,
    pub delay: Duration,
    pub duration: Duration,
    pub location: HudLocation,
}

#[derive(Default, Debug, Clone)]
pub struct HudText {
    pub text: String,
    pub duration: Range<f32>,
}

#[derive(Resource, Default)]
struct HudState {
    current_texts: HashMap<HudLocation, HudText>,
}

fn update_ui(
    mut contexts: EguiContexts,
    state: Res<HudState>,
    time: Res<Time>,
    _window: Single<&mut Window, With<PrimaryWindow>>,
) -> Result {
    let ctx = contexts.ctx_mut()?;

    let rect = ctx.content_rect().shrink2(MARGIN);

    egui::Area::new(egui::Id::new("overlay"))
        .fixed_pos(rect.min)
        .order(egui::Order::Background)
        .show(ctx, |ui| {
            ui.set_min_size(rect.size());
            for (text, corner) in [
                (HudLocation::TopLeft, egui::Align2::LEFT_TOP),
                (HudLocation::TopRight, egui::Align2::RIGHT_TOP),
                (HudLocation::BottomLeft, egui::Align2::LEFT_BOTTOM),
                (HudLocation::InfoText, egui::Align2::RIGHT_BOTTOM),
            ] {
                let info = state.current_texts.get(&text).cloned().unwrap_or_default();
                let on = info.duration.contains(&time.elapsed_secs());
                let t = ctx.animate_bool_with_time(egui::Id::new(corner), on, 1.0);
                if t > 0.0 {
                    let quad = egui::Rect::from_two_pos(corner.pos_in_rect(&rect), rect.center());
                    let layout = if corner.y() == egui::Align::Min {
                        egui::Layout::top_down(corner.x())
                    } else {
                        egui::Layout::bottom_up(corner.x())
                    };

                    let color = ui.visuals().text_color().linear_multiply(t);

                    ui.scope_builder(egui::UiBuilder::new().max_rect(quad).layout(layout), |ui| {
                        ui.heading(egui::RichText::new(info.text).color(color));
                    });
                }
            }
        });
    Ok(())
}

fn spawn_toast(
    mut state: ResMut<HudState>,
    time: Res<Time>,
    mut reader: MessageReader<SetHudText>,
) {
    for msg in reader.read() {
        info!("MSG: {}", msg.text);
        let now = time.elapsed_secs();
        let start = now + msg.delay.as_secs_f32();
        let stop = start + msg.duration.as_secs_f32();
        state.current_texts.insert(
            msg.location,
            HudText {
                text: msg.text.clone(),
                duration: (start..stop),
            },
        );
    }
}

impl Plugin for EguiUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(EguiPlugin::default());
        // `EguiPlugin::build` has already run and inserted the resource, so this
        // lands before any camera is spawned in `Startup`.
        app.world_mut()
            .resource_mut::<EguiGlobalSettings>()
            .auto_create_primary_context = false;
        app.add_systems(Startup, load_font)
            .add_systems(EguiPrimaryContextPass, (setup_egui, update_ui).chain())
            .add_message::<SetHudText>()
            .add_systems(Update, spawn_toast.run_if(on_message::<SetHudText>))
            .insert_resource(HudState::default());
    }
}
