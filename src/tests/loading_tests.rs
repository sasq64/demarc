use std::time::Instant;

use bevy::ecs::system::RunSystemOnce;

use super::*;
use crate::emu_file::{FileSource, GameInfo, UrlList};
use crate::frontend::spawn_emulator;

/// What the loading systems read, headless.
fn world(db: &[&'static str]) -> (World, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::new();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<AppSettings>();
    world.init_resource::<GlobalSettings>();
    world.init_resource::<CrossFade>();
    world.init_resource::<Time>();
    world.init_resource::<Messages<SetHudText>>();
    world.init_resource::<Messages<LoadFile>>();
    world.init_resource::<Messages<LoadFinished>>();
    world.insert_resource(Navigator::new());
    world.insert_resource(Playlists::load(dir.path()));
    let files: Vec<EmuFile> = db.iter().map(|title| entry(title)).collect();
    world
        .resource_mut::<AppSettings>()
        .files
        .insert("db".into(), Vec::leak(files));
    (world, dir)
}

/// An entry whose download is refused at once, so a load of it fails fast
/// without leaving the host.
fn entry(title: &'static str) -> EmuFile {
    EmuFile {
        path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
        game_info: GameInfo {
            title,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn spawn(world: &mut World, index: usize) -> Entity {
    let entity = spawn_emulator(world, false, None, false, index, None);
    world.get_mut::<Emulator>(entity).unwrap().run_next = false;
    entity
}

fn emu_mut(world: &mut World, entity: Entity) -> Mut<'_, Emulator> {
    world.get_mut::<Emulator>(entity).unwrap()
}

fn settings(world: &mut World) -> Mut<'_, AppSettings> {
    world.resource_mut::<AppSettings>()
}

fn handle(world: &mut World) {
    world.run_system_once(handle_loading).unwrap();
}

/// Titles and targets of the `LoadFile`s sent so far.
fn loads(world: &mut World) -> Vec<(&'static str, Option<usize>)> {
    world
        .resource_mut::<Messages<LoadFile>>()
        .drain()
        .map(|l| (l.emu_file.game_info.title, l.target_emulator))
        .collect()
}

fn hud(world: &mut World) -> Vec<SetHudText> {
    world
        .resource_mut::<Messages<SetHudText>>()
        .drain()
        .collect()
}

/// Steps `entity` once in direction `d` from `current_game` and returns what
/// it asked to load.
fn step(world: &mut World, entity: Entity, current: isize, d: isize) -> Vec<(&'static str, Option<usize>)> {
    settings(world).current_game = current;
    let mut emu = emu_mut(world, entity);
    emu.run_next = d > 0;
    emu.run_prev = d < 0;
    handle(world);
    loads(world)
}

#[test]
fn next_and_prev_walk_the_default_db() {
    let (mut world, _dir) = world(&["a", "b", "c"]);
    let entity = spawn(&mut world, 2);

    assert_eq!(step(&mut world, entity, 0, 1), [("b", Some(2))]);
    assert_eq!(world.resource::<AppSettings>().current_game, 1);
    let emu = world.get::<Emulator>(entity).unwrap();
    assert!(!emu.run_next && !emu.run_prev);

    assert_eq!(step(&mut world, entity, 1, -1), [("a", Some(2))]);
    assert_eq!(world.resource::<AppSettings>().current_game, 0);
}

#[test]
fn the_ends_of_the_db_stop_outside_tv_mode() {
    let (mut world, _dir) = world(&["a", "b", "c"]);
    let entity = spawn(&mut world, 0);

    assert!(step(&mut world, entity, 2, 1).is_empty());
    assert!(step(&mut world, entity, 0, -1).is_empty());
    assert_eq!(world.resource::<AppSettings>().current_game, 0);
    // The request is left standing.
    assert!(world.get::<Emulator>(entity).unwrap().run_prev);
}

#[test]
fn tv_mode_wraps_around_the_db() {
    let (mut world, _dir) = world(&["a", "b", "c"]);
    settings(&mut world).tv_mode = true;
    let entity = spawn(&mut world, 0);

    assert_eq!(step(&mut world, entity, 2, 1), [("a", Some(0))]);
    assert_eq!(world.resource::<AppSettings>().current_game, 0);
    assert_eq!(step(&mut world, entity, 0, -1), [("c", Some(0))]);
    assert_eq!(world.resource::<AppSettings>().current_game, 2);
}

#[test]
fn an_empty_db_loads_nothing() {
    let (mut world, _dir) = world(&[]);
    settings(&mut world).tv_mode = true;
    let entity = spawn(&mut world, 0);

    assert!(step(&mut world, entity, 0, 1).is_empty());
}

#[test]
fn info_delay_starts_running_and_shows_info_when_maximized() {
    let (mut world, _dir) = world(&[]);
    let entity = spawn(&mut world, 0);

    emu_mut(&mut world, entity).state = EmuState::InfoDelay;
    handle(&mut world);
    assert!(matches!(
        world.get::<Emulator>(entity).unwrap().state,
        EmuState::Running
    ));
    assert!(hud(&mut world).is_empty());

    settings(&mut world).show_info = true;
    settings(&mut world).maximized = true;
    emu_mut(&mut world, entity).state = EmuState::InfoDelay;
    handle(&mut world);
    assert_eq!(hud(&mut world).len(), 2);
}

#[test]
fn load_file_starts_a_load_in_the_targeted_view_only() {
    let _app = task_pools();
    let (mut world, _dir) = world(&[]);
    let first = spawn(&mut world, 0);
    let second = spawn(&mut world, 1);
    settings(&mut world).current_emu = 1;

    world.write_message(LoadFile {
        emu_file: entry("a"),
        target_emulator: None,
    });
    world.run_system_once(load_file).unwrap();

    assert!(matches!(
        world.get::<Emulator>(first).unwrap().state,
        EmuState::Stopped
    ));
    assert!(matches!(
        world.get::<Emulator>(second).unwrap().state,
        EmuState::Loading
    ));

    world.write_message(LoadFile {
        emu_file: entry("b"),
        target_emulator: Some(0),
    });
    world.run_system_once(load_file).unwrap();
    assert!(matches!(
        world.get::<Emulator>(first).unwrap().state,
        EmuState::Loading
    ));
}

/// Starts a load of an unreachable entry in `entity` and runs `handle_loading`
/// until it has failed.
fn fail_load(world: &mut World, entity: Entity) {
    let global = GlobalSettings::default();
    emu_mut(world, entity).start_load(&entry("Unreachable"), None, &global);
    let deadline = Instant::now() + Duration::from_secs(30);
    while !matches!(world.get::<Emulator>(entity).unwrap().state, EmuState::Stopped) {
        assert!(Instant::now() < deadline, "load never finished");
        handle(world);
        std::thread::yield_now();
    }
}

/// Spins up the task pools the load jobs run on.
fn task_pools() -> App {
    let mut app = App::new();
    app.add_plugins(MinimalPlugins);
    app.update();
    app
}

#[test]
fn a_failed_load_reports_the_error_and_stops() {
    let _app = task_pools();
    let (mut world, _dir) = world(&[]);
    let entity = spawn(&mut world, 0);

    fail_load(&mut world, entity);

    let hud = hud(&mut world);
    assert_eq!(hud.len(), 1);
    assert!(hud[0].text.starts_with("Could not load Unreachable:"));
    assert!(matches!(hud[0].location, HudLocation::Error));
    let emu = world.get::<Emulator>(entity).unwrap();
    assert!(!emu.run_next && !emu.run_prev);
    assert!(
        world
            .resource_mut::<Messages<LoadFinished>>()
            .drain()
            .next()
            .is_none()
    );
}

#[test]
fn a_failed_load_in_tv_mode_moves_on() {
    let _app = task_pools();
    let (mut world, _dir) = world(&[]);
    settings(&mut world).tv_mode = true;
    let entity = spawn(&mut world, 0);

    fail_load(&mut world, entity);

    assert!(hud(&mut world).is_empty());
    assert!(world.get::<Emulator>(entity).unwrap().run_next);
}
