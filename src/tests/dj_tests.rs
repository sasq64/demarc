use bevy::ecs::system::RunSystemOnce;
use clap::Parser;

use super::*;

fn args(extra: &[&str]) -> Args {
    Args::parse_from(std::iter::once(&"demarc").chain(extra))
}

fn world() -> World {
    let mut world = World::new();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<EguiGlobalSettings>();
    world.run_system_once(setup_dj).unwrap();
    world
}

fn dj(world: &World) -> &DjWindow {
    world.resource::<DjWindow>()
}

fn dj_view(world: &mut World) -> PostProcess {
    world
        .query_filtered::<&PostProcess, With<DjView>>()
        .single(world)
        .unwrap()
        .clone()
}

/// Spawns an emulator view with its own image, as the spare when `crossfade`.
fn spawn_emu(world: &mut World, crossfade: bool, aspect: f32) -> Handle<Image> {
    let mut images = world.resource_mut::<Assets<Image>>();
    let mut emu = Emulator::new(&mut images, None, false, false);
    emu.is_crossfade = crossfade;
    let image = emu.image.clone();
    world.spawn((
        emu,
        PostProcess {
            source: image.clone(),
            aspect,
            aspect_tweak: 1.1,
            used: UVec2::new(320, 200),
            view: ViewRect {
                position: UVec2::ZERO,
                size: UVec2::ZERO,
                active: true,
            },
            alpha: 0.0,
            raw: false,
            no_crt: false,
        },
    ));
    image
}

#[test]
fn focus_is_only_had_by_an_open_focused_window() {
    let mut window = DjWindow {
        window: Entity::PLACEHOLDER,
        focused: false,
    };
    assert!(!has_focus(None));
    assert!(!has_focus(Some(&window)));
    window.focused = true;
    assert!(has_focus(Some(&window)));
}

#[test]
fn enabled_by_dj_mode_except_headless() {
    assert!(!enabled(&args(&[])));
    assert!(enabled(&args(&["--dj-mode"])));
    assert!(!enabled(&args(&["--dj-mode", "--headless"])));
}

#[test]
fn setup_opens_the_window_with_its_cameras_and_view() {
    let mut world = world();

    let window = dj(&world).window;
    assert!(!dj(&world).focused);
    assert_eq!(world.get::<Window>(window).unwrap().title, "Demarc DJ");

    let mut cameras = world.query_filtered::<&Camera, With<DjCamera>>();
    assert_eq!(cameras.single(&world).unwrap().order, 0);
    let ui = world.resource::<FocusedNonWindowEguiContext>().0;
    assert!(world.get::<PrimaryEguiContext>(ui).is_some());
    assert!(
        !world
            .resource::<EguiGlobalSettings>()
            .enable_focused_non_window_context_updates
    );

    let view = dj_view(&mut world);
    assert!(view.raw);
    assert_eq!(view.alpha, 1.0);
}

#[test]
fn focus_follows_the_window() {
    let mut world = world();
    let window = dj(&world).window;

    world.get_mut::<Window>(window).unwrap().focused = true;
    world.run_system_once(track_focus).unwrap();
    assert!(dj(&world).focused);

    world.get_mut::<Window>(window).unwrap().focused = false;
    world.run_system_once(track_focus).unwrap();
    assert!(!dj(&world).focused);
}

#[test]
fn the_view_shows_the_spare_at_the_window_size() {
    let mut world = world();
    spawn_emu(&mut world, false, 1.0);
    let spare = spawn_emu(&mut world, true, 4.0 / 3.0);

    world.run_system_once(update_dj_view).unwrap();

    let window = dj(&world).window;
    let size = world.get::<Window>(window).unwrap().physical_size();
    let view = dj_view(&mut world);
    assert_eq!(view.view.size, size);
    assert_eq!(view.source, spare);
    assert_eq!(view.aspect, 4.0 / 3.0);
    assert_eq!(view.aspect_tweak, 1.1);
    assert_eq!(view.used, UVec2::new(320, 200));
}

#[test]
fn without_a_spare_the_view_has_no_source() {
    let mut world = world();
    spawn_emu(&mut world, false, 1.0);

    world.run_system_once(update_dj_view).unwrap();

    let view = dj_view(&mut world);
    assert_eq!(view.source, Handle::default());
    assert_eq!(view.aspect, 0.0);
}
