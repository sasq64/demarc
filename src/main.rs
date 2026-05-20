use bevy::{
    camera::{RenderTarget, visibility::RenderLayers},
    image::Image,
    prelude::*,
    render::render_resource::TextureFormat,
};
use rand::RngExt;

mod post_process;

use post_process::{PostProcess, PostProcessPlugin};

pub struct GamePlugin {}

#[derive(Component)]
pub struct Velocity(Vec2);

const LOW_RES_WIDTH: u32 = 640 / 2;
const LOW_RES_HEIGHT: u32 = 360 / 2;

fn setup_cameras(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let image = Image::new_target_texture(
        LOW_RES_WIDTH,
        LOW_RES_HEIGHT,
        TextureFormat::bevy_default(),
        None,
    );
    let low_res = images.add(image);

    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        RenderTarget::Image(low_res.clone().into()),
    ));

    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            ..default()
        },
        PostProcess { source: low_res },
        RenderLayers::layer(1),
    ));
}

fn spawn_sprites(asset_server: Res<AssetServer>, mut commands: Commands) {
    let image: Handle<Image> = asset_server.load("face.png");
    let mut rng = rand::rng();

    for _ in 0..1000 {
        let v = Vec2::new(rng.random_range(-5.0..5.0), rng.random_range(-5.0..5.0));

        let mut _entity = commands.spawn((
            Transform::from_xyz(0.0, 0.0, 0.0),
            Sprite::from_image(image.clone()),
            Velocity(v),
        ));
    }
}

fn update_sprites(mut sprites: Query<(&mut Transform, &Velocity)>) {
    for (mut tx, vel) in sprites.iter_mut() {
        tx.translation += vel.0.extend(0.0);
        if tx.translation.x > 640.0 {
            tx.translation.x -= 1280.0;
        }
        if tx.translation.y > 3600.0 {
            tx.translation.y -= 720.0;
        }
    }
}

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, (setup_cameras, spawn_sprites));
        app.add_systems(Update, (update_sprites,));
    }
}

fn main() {
    let primary_window = Some(Window {
        title: "Rupix".into(),
        resolution: (1280, 720).into(),
        resizable: false,
        ..Default::default()
    });

    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window,
                ..Default::default()
            }),
            GamePlugin {},
            PostProcessPlugin,
        ))
        .run();
}
