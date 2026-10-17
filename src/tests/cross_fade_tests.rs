use std::time::Duration;

use bevy::ecs::system::RunSystemOnce;
use bevy::tasks::{IoTaskPool, TaskPool};
use clap::Parser;

use super::*;
use crate::fake_backend::FakeBackend;

fn args(extra: &[&str]) -> Args {
    Args::parse_from(std::iter::once(&"demarc").chain(extra))
}

fn world(extra: &[&str]) -> World {
    IoTaskPool::get_or_init(TaskPool::new);
    let mut world = World::new();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<Time>();
    world.init_resource::<CrossFade>();
    world.init_resource::<Messages<LoadFinished>>();
    world.init_resource::<Messages<CmdMessage>>();
    world.insert_resource(args(extra));
    world
}

/// Spawns view `index` running a fake core.
fn spawn_view(world: &mut World, index: usize, cell: Option<GridCell>) -> (Entity, FakeBackend) {
    let core = FakeBackend::default();
    let entity = spawn_emulator(world, false, None, false, index, cell);
    world.get_mut::<Emulator>(entity).unwrap().core = core.boxed();
    (entity, core)
}

/// A world with the spare and one view at index 0, both running fake cores.
fn setup(extra: &[&str]) -> (World, Entity, Entity, FakeBackend) {
    let mut world = world(extra);
    spawn_spare(&mut world);
    let spare = world.resource::<CrossFade>().spare.unwrap();
    let core = FakeBackend::default();
    world.get_mut::<Emulator>(spare).unwrap().core = core.boxed();
    let (view, _) = spawn_view(&mut world, 0, None);
    (world, spare, view, core)
}

fn redirect(world: &mut World, index: usize) -> usize {
    world
        .run_system_once(move |mut state: ResMut<CrossFade>, mut views: Views| {
            redirect_load(&mut state, &mut views, index)
        })
        .unwrap()
}

fn finish_load(world: &mut World, entity: Entity) {
    world.write_message(LoadFinished(entity));
    world.run_system_once(start_fade).unwrap();
    // A one-shot system's reader starts at the oldest message still buffered.
    world.resource_mut::<Messages<LoadFinished>>().clear();
}

fn fade(world: &mut World) {
    world.run_system_once(run_fade).unwrap();
}

fn advance(world: &mut World, secs: f32) {
    world
        .resource_mut::<Time>()
        .advance_by(Duration::from_secs_f32(secs));
}

fn emu(world: &World, entity: Entity) -> &Emulator {
    world.get::<Emulator>(entity).unwrap()
}

fn index(world: &World, entity: Entity) -> usize {
    world.get::<EmuView>(entity).unwrap().index
}

fn alpha(world: &World, entity: Entity) -> f32 {
    world.get::<PostProcess>(entity).unwrap().alpha
}

fn state(world: &World) -> &CrossFade {
    world.resource::<CrossFade>()
}

/// Redirects a load for view 0 into the spare and lands it.
fn load(world: &mut World, spare: Entity) {
    assert_eq!(redirect(world, 0), CROSSFADE_INDEX);
    finish_load(world, spare);
}

#[test]
fn no_spare_without_cross_fade() {
    let mut world = world(&[]);
    spawn_spare(&mut world);
    assert!(state(&world).spare.is_none());
    assert_eq!(world.query::<&Emulator>().iter(&world).count(), 0);
}

#[test]
fn the_spare_starts_hidden_silent_and_idle() {
    let mut world = world(&["--cross-fade"]);
    spawn_spare(&mut world);

    let spare = state(&world).spare.unwrap();
    let emu = emu(&world, spare);
    assert!(emu.is_crossfade);
    assert!(!emu.run_next);
    assert_eq!(emu.volume(), 0.0);
    assert_eq!(alpha(&world, spare), 0.0);
    assert_eq!(index(&world, spare), CROSSFADE_INDEX);
    assert!(world.get::<GridCell>(spare).is_none());
}

#[test]
fn the_spare_has_a_grid_cell_in_grid_mode() {
    let mut world = world(&["--cross-fade", "--grid=2x2"]);
    spawn_spare(&mut world);
    let spare = state(&world).spare.unwrap();
    assert!(world.get::<GridCell>(spare).is_some());
}

#[test]
fn loads_pass_through_without_a_spare() {
    let mut world = world(&[]);
    spawn_view(&mut world, 0, None);
    assert_eq!(redirect(&mut world, 0), 0);
    assert!(state(&world).origin.is_none());
}

#[test]
fn loads_for_the_spare_itself_are_not_redirected() {
    let (mut world, ..) = setup(&["--cross-fade"]);
    assert_eq!(redirect(&mut world, CROSSFADE_INDEX), CROSSFADE_INDEX);
    assert!(state(&world).origin.is_none());
}

#[test]
fn loads_for_an_unknown_view_pass_through() {
    let (mut world, ..) = setup(&["--cross-fade"]);
    assert_eq!(redirect(&mut world, 7), 7);
    assert!(state(&world).origin.is_none());
}

#[test]
fn a_load_is_redirected_into_the_spare_and_takes_the_views_cell() {
    let mut world = world(&["--cross-fade", "--grid=2x1"]);
    spawn_spare(&mut world);
    let spare = state(&world).spare.unwrap();
    let cells = grid_cells(2, 1);
    let (view, _) = spawn_view(&mut world, 1, Some(cells[1]));
    world.resource_mut::<CrossFade>().armed = true;

    assert_eq!(redirect(&mut world, 1), CROSSFADE_INDEX);

    assert_eq!(state(&world).origin, Some(view));
    assert!(!state(&world).armed);
    assert_eq!(world.get::<GridCell>(spare).unwrap().offset, cells[1].offset);
}

#[test]
fn only_the_spares_load_starts_the_clock() {
    let (mut world, spare, view, _) = setup(&["--cross-fade"]);

    finish_load(&mut world, spare);
    assert!(state(&world).loaded_at.is_none(), "no load was redirected");

    redirect(&mut world, 0);
    finish_load(&mut world, view);
    assert!(state(&world).loaded_at.is_none());

    advance(&mut world, 1.5);
    finish_load(&mut world, spare);
    assert_eq!(state(&world).loaded_at, Some(1.5));
}

#[test]
fn the_fade_waits_out_the_delay_then_ramps_and_hands_over() {
    let (mut world, spare, view, _) = setup(&["--cross-fade"]);
    load(&mut world, spare);

    advance(&mut world, DELAY_TIME - 0.1);
    fade(&mut world);
    assert!(state(&world).fade_at.is_none());
    assert_eq!(alpha(&world, spare), 0.0);

    advance(&mut world, 0.1);
    fade(&mut world);
    assert!(state(&world).fade_at.is_some());

    advance(&mut world, FADE_TIME / 4.0);
    fade(&mut world);
    let elapsed = world.resource::<Time>().elapsed_secs_f64();
    assert!((alpha(&world, spare) - 0.25).abs() < 1e-3);
    assert!((emu(&world, spare).volume() - 0.5).abs() < 1e-3);
    assert!((emu(&world, view).volume() - 0.75f32.sqrt()).abs() < 1e-3);
    assert!((emu(&world, spare).start_time - (elapsed - (FADE_TIME / 4.0) as f64)).abs() < 1e-3);

    advance(&mut world, FADE_TIME);
    fade(&mut world);

    assert_eq!(index(&world, spare), 0);
    assert_eq!(index(&world, view), CROSSFADE_INDEX);
    assert_eq!(alpha(&world, spare), 1.0);
    assert_eq!(alpha(&world, view), 0.0);
    assert_eq!(emu(&world, spare).volume(), 1.0);
    assert_eq!(emu(&world, view).volume(), 0.0);
    assert!(!emu(&world, spare).is_crossfade);
    assert!(emu(&world, view).is_crossfade);
    assert!(matches!(emu(&world, spare).state, EmuState::InfoDelay));
    assert!(emu(&world, view).core.is_none());

    let state = state(&world);
    assert_eq!(state.spare, Some(view));
    assert!(state.origin.is_none() && state.loaded_at.is_none() && state.fade_at.is_none());
}

#[test]
fn nothing_fades_before_a_load_lands() {
    let (mut world, spare, ..) = setup(&["--cross-fade"]);
    redirect(&mut world, 0);
    advance(&mut world, DELAY_TIME + FADE_TIME);
    fade(&mut world);
    assert!(state(&world).fade_at.is_none());
    assert_eq!(index(&world, spare), CROSSFADE_INDEX);
}

#[test]
fn a_quiet_picture_holds_the_fade_back() {
    let (mut world, spare, _, core) = setup(&["--cross-fade", "--cross-fade-activity=0.3"]);
    core.lock().activity = 0.1;
    load(&mut world, spare);

    advance(&mut world, DELAY_TIME + 1.0);
    fade(&mut world);
    assert!(state(&world).fade_at.is_none());

    core.lock().activity = 0.4;
    fade(&mut world);
    assert!(state(&world).fade_at.is_some());
}

#[test]
fn dj_mode_waits_for_the_fade_to_be_asked_for() {
    let (mut world, spare, ..) = setup(&["--cross-fade", "--dj-mode"]);
    load(&mut world, spare);

    advance(&mut world, DELAY_TIME * 10.0);
    fade(&mut world);
    assert!(state(&world).fade_at.is_none());

    world.write_message(CmdMessage(Cmd::StartOther));
    world.run_system_once(arm_fade).unwrap();
    fade(&mut world);
    assert!(state(&world).fade_at.is_some());
}

#[test]
fn an_armed_fade_skips_the_delay_and_activity_check() {
    let (mut world, spare, _, core) = setup(&["--cross-fade", "--cross-fade-activity=0.9"]);
    core.lock().activity = 0.0;
    load(&mut world, spare);
    world.resource_mut::<CrossFade>().armed = true;

    fade(&mut world);
    assert_eq!(state(&world).fade_at, Some(0.0));
}

#[test]
fn only_start_other_arms_the_fade() {
    let mut world = world(&["--cross-fade"]);
    world.write_message(CmdMessage(Cmd::NextFile));
    world.run_system_once(arm_fade).unwrap();
    assert!(!state(&world).armed);

    world.write_message(CmdMessage(Cmd::StartOther));
    world.run_system_once(arm_fade).unwrap();
    assert!(state(&world).armed);
}

#[test]
fn a_new_load_before_the_fade_replaces_the_pending_one() {
    let (mut world, spare, view, _) = setup(&["--cross-fade"]);
    load(&mut world, spare);

    assert_eq!(redirect(&mut world, 0), CROSSFADE_INDEX);

    assert!(state(&world).loaded_at.is_none());
    assert_eq!(state(&world).spare, Some(spare));
    assert_eq!(state(&world).origin, Some(view));
}

#[test]
fn a_new_load_mid_fade_hands_over_at_once() {
    let (mut world, spare, view, _) = setup(&["--cross-fade"]);
    load(&mut world, spare);
    advance(&mut world, DELAY_TIME);
    fade(&mut world);
    assert!(state(&world).fade_at.is_some());

    // The spare now owns view 0, so the next load goes to the old view.
    assert_eq!(redirect(&mut world, 0), CROSSFADE_INDEX);

    assert_eq!(index(&world, spare), 0);
    assert_eq!(state(&world).spare, Some(view));
    assert_eq!(state(&world).origin, Some(spare));
}
