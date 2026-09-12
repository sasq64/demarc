//! Loading the next release into a spare, off-screen emulator and swapping it
//! onto the screen once it is ready — the plumbing a cross fade needs, without
//! the fade itself.
//!
//! `load_prepared` drops the running core before it builds the new one, so an
//! emulator that loads cannot keep showing what it was showing. With
//! `--cross-fade` an extra emulator entity (the *spare*) is spawned, every
//! advance is diverted into it, and when the load lands the two entities trade
//! roles: the spare takes the origin's view index and alpha, the origin becomes
//! the new spare.
//!
//! There is one spare, so under `--grid` only the first emulator asking to
//! advance in a frame is diverted; while that load runs the others load into
//! themselves as they always did. Cross fade is aimed at the single view case.

use bevy::prelude::*;

use crate::config::Args;
use crate::emulator::Emulator;
use crate::frontend::{EmuView, GridCell, grid_cells, setup_frontend, spawn_emulator};
use crate::post_process::PostProcess;

/// [`EmuView::index`] of the spare while it is off screen — past anything
/// `current_emu` or `mouse_index` is ever compared against.
pub(crate) const CROSSFADE_INDEX: usize = usize::MAX;

/// One emulator finished a load this frame.
#[derive(Message)]
pub struct LoadFinished(pub Entity);

#[derive(Resource, Default)]
struct CrossFade {
    spare: Option<Entity>,
    /// The view the load in flight was taken from.
    origin: Option<Entity>,
}

pub struct CrossFadePlugin;

/// Spawn the spare: an ordinary emulator view, off screen and not asking to
/// load anything by itself.
fn spawn_spare(world: &mut World) {
    let args = world.resource::<Args>();
    if !args.cross_fade {
        return;
    }
    let (color_cycle, max_time, speed_test) = (args.color_cycle, args.max_time, args.speed_test);
    // A grid cell from the start, so the archetype is fixed for the run and
    // taking over a view's rectangle only writes the cell's value.
    let cell = args.grid.map(|(cols, rows)| grid_cells(cols, rows)[0]);

    let entity = spawn_emulator(
        world,
        color_cycle,
        max_time,
        speed_test,
        CROSSFADE_INDEX,
        cell,
    );

    let mut emu = world.get_mut::<Emulator>(entity).expect("just spawned");
    emu.is_crossfade = true;
    emu.run_next = false;
    world
        .get_mut::<PostProcess>(entity)
        .expect("just spawned")
        .alpha = 0.0;

    world.resource_mut::<CrossFade>().spare = Some(entity);
}

/// Move a pending advance off the view that asked for it and onto the spare, so
/// `handle_loading` drives the load there instead.
fn hijack_load(
    mut state: ResMut<CrossFade>,
    mut emus: Query<(Entity, &mut Emulator, &EmuView, Option<&mut GridCell>)>,
) {
    let Some(spare) = state.spare else {
        return;
    };
    let Ok((_, emu, ..)) = emus.get(spare) else {
        return;
    };
    if emu.is_loading() || emu.run_next || emu.run_prev {
        return;
    }

    let Some((origin, advance, index, cell)) = emus
        .iter()
        .find(|(_, emu, ..)| !emu.is_crossfade && (emu.run_next || emu.run_prev))
        .map(|(e, emu, view, cell)| (e, (emu.run_next, emu.run_prev), view.index, cell.copied()))
    else {
        return;
    };

    if let Ok((_, mut emu, ..)) = emus.get_mut(origin) {
        emu.run_next = false;
        emu.run_prev = false;
    }
    let Ok((_, mut emu, _, spare_cell)) = emus.get_mut(spare) else {
        return;
    };
    (emu.run_next, emu.run_prev) = advance;
    // Same rectangle as the view it is loading for, ready for the fade.
    if let (Some(mut spare_cell), Some(cell)) = (spare_cell, cell) {
        *spare_cell = cell;
    }
    debug!("Hijacked load for view {index} into the cross fade emulator");
    state.origin = Some(origin);
}

/// The spare finished loading: trade roles with the view it loaded for.
fn finish_crossfade(
    mut finished: MessageReader<LoadFinished>,
    mut state: ResMut<CrossFade>,
    mut emus: Query<(&mut Emulator, &mut EmuView, &mut PostProcess)>,
) {
    for LoadFinished(entity) in finished.read() {
        let (Some(spare), Some(origin)) = (state.spare, state.origin) else {
            continue;
        };
        if *entity != spare {
            continue;
        }
        let Ok(
            [
                (mut o_emu, mut o_view, mut o_pp),
                (mut s_emu, mut s_view, mut s_pp),
            ],
        ) = emus.get_many_mut([origin, spare])
        else {
            continue;
        };

        s_view.index = o_view.index;
        o_view.index = CROSSFADE_INDEX;
        std::mem::swap(&mut o_pp.alpha, &mut s_pp.alpha);
        o_emu.is_crossfade = true;
        s_emu.is_crossfade = false;
        o_emu.core = None;

        debug!("Cross fade emulator took over view {}", s_view.index);
        state.spare = Some(origin);
        state.origin = None;
    }
}

impl Plugin for CrossFadePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CrossFade>()
            .add_message::<LoadFinished>()
            .add_systems(Startup, spawn_spare.after(setup_frontend))
            .add_systems(
                Update,
                (
                    // Between everything that arms an advance and the system
                    // that acts on one, so a load is never started on the view
                    // it was requested from.
                    hijack_load
                        .after(crate::commands::handle_cmd)
                        .after(crate::frontend::run_frontend)
                        .before(crate::frontend::handle_loading),
                    finish_crossfade.after(crate::frontend::handle_loading),
                ),
            );
    }
}
