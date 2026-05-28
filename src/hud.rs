use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowResized};
use bevy_tweening::lens::TextColorLens;
use bevy_tweening::{CycleCompletedEvent, Delay, Tween, TweenAnim};

#[derive(Component)]
pub struct InfoText;

#[derive(Default)]
pub enum ToastType {
    #[default]
    InfoText,
    BottomLeft,
}

#[derive(Default, Message)]
pub struct SpawnToast {
    pub text: String,
    pub delay: Duration,
    pub duration: Duration,
    pub toast_type: ToastType,
}

#[derive(Resource, Default)]
struct HudState {
    current_toast: Option<Entity>,
}

#[derive(Component)]
struct RelativeTextSize {
    /// Font size as a fraction of window height (e.g. 0.05 = 5% of height)
    fraction: f32,
}

fn update_relative_text_size(
    mut resize_events: MessageReader<WindowResized>,
    mut query: Query<(&RelativeTextSize, &mut TextFont)>,
) {
    for event in resize_events.read() {
        for (rel, mut text_font) in &mut query {
            text_font.font_size = event.height * rel.fraction;
        }
    }
}
fn spawn_toast(
    mut commands: Commands,
    mut state: ResMut<HudState>,
    mut reader: MessageReader<SpawnToast>,
    asset_server: Res<AssetServer>,
    window: Single<&mut Window, With<PrimaryWindow>>,
) {
    let font = asset_server.load("font.ttf");
    warn!("SIZE {} {}", window.physical_width(), window.width());
    let font_size = window.physical_width() as f32 / 50.0;
    for msg in reader.read() {
        let show_tween = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_millis(500),
            TextColorLens {
                start: Color::srgba(0., 0., 0., 0.),
                end: Color::WHITE,
            },
        );
        let hide_tween = Tween::new(
            EaseFunction::QuadraticInOut,
            Duration::from_millis(500),
            TextColorLens {
                start: Color::WHITE,
                end: Color::srgba(0., 0., 0., 0.),
            },
        )
        .with_cycle_completed_event(true);

        let tween = if msg.delay == Duration::ZERO {
            show_tween.then(Delay::new(msg.duration)).then(hide_tween)
        } else {
            Delay::new(msg.delay)
                .then(show_tween)
                .then(Delay::new(msg.duration))
                .then(hide_tween)
        };

        match msg.toast_type {
            ToastType::InfoText => {
                if let Some(toast) = state.current_toast {
                    commands.entity(toast).despawn();
                }
                let entity = commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        right: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(60.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    InfoText,
                    TextFont {
                        font: font.clone(),
                        font_size,
                        ..default()
                    },
                    RelativeTextSize { fraction: 0.05 },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ));
                state.current_toast = Some(entity.id());
            }
            ToastType::BottomLeft => {
                let _entity = commands.spawn((
                    Node {
                        //width: Val::Px(400.0),
                        position_type: PositionType::Absolute,
                        bottom: Val::Px(0.0),
                        left: Val::Px(0.0),
                        margin: UiRect::all(Val::Px(60.0)),
                        ..default()
                    },
                    Text::new(&msg.text),
                    // TextShadow {
                    //     offset: Vec2::new(2.0, -2.0),
                    //     color: Color::BLACK,
                    // },
                    TextFont {
                        font: font.clone(),
                        font_size: 64.0,
                        ..default()
                    },
                    TextColor(Color::srgba(0.0, 0.0, 0.0, 0.0)),
                    TextLayout {
                        justify: Justify::Right,
                        linebreak: LineBreak::WordBoundary,
                    },
                    TweenAnim::new(tween),
                ));
            }
        }
    }
}

fn handle_tween_done(mut commands: Commands, mut reader: MessageReader<CycleCompletedEvent>) {
    for msg in reader.read() {
        info!("DESPAWN");
        commands.entity(msg.anim_entity).despawn();
    }
}

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<SpawnToast>()
            .insert_resource(HudState::default())
            .add_systems(
                Update,
                (
                    spawn_toast.run_if(on_message::<SpawnToast>),
                    update_relative_text_size.run_if(on_message::<WindowResized>),
                    handle_tween_done.run_if(on_message::<CycleCompletedEvent>),
                ),
            );
    }
}
