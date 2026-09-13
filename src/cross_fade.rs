//! Loading the next release into a spare, off-screen emulator and fading it in
//! over the one it replaces.
//!
//! `load_prepared` drops the running core before it builds the new one, so an
//! emulator that loads cannot keep showing what it was showing. With
//! `--cross-fade` an extra emulator entity (the *spare*) is spawned and every
//! advance is diverted into it. Once the load lands the spare keeps running off
//! screen for [`DELAY_TIME`], fades up over [`FADE_TIME`], and then the two
//! entities trade roles: the spare takes the origin's view index, the origin
//! becomes the new spare.
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

/// Seconds the loaded release runs off screen before the fade starts, so what
/// fades up is the demo running rather than its first black frames.
const DELAY_TIME: f32 = 5.0;

/// Seconds the fade itself takes.
const FADE_TIME: f32 = 1.5;

/// One emulator finished a load this frame.
#[derive(Message)]
pub struct LoadFinished(pub Entity);

#[derive(Resource, Default)]
struct CrossFade {
    spare: Option<Entity>,
    /// The view the load in flight was taken from.
    origin: Option<Entity>,
    /// When the spare's load finished; `DELAY_TIME` then `FADE_TIME` run from
    /// here. `Some` exactly while the fade is pending or running.
    loaded_at: Option<f64>,
}

type Views<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static mut Emulator,
        &'static mut EmuView,
        &'static mut PostProcess,
        Option<&'static mut GridCell>,
    ),
>;

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

/// Trade roles: the spare becomes the view it loaded for, at full opacity, and
/// the origin drops its core and goes off screen as the new spare.
fn swap_roles(
    state: &mut CrossFade,
    views: &mut Query<(
        Entity,
        &mut Emulator,
        &mut EmuView,
        &mut PostProcess,
        Option<&mut GridCell>,
    )>,
) {
    let (Some(spare), Some(origin)) = (state.spare, state.origin) else {
        return;
    };
    let Ok(
        [
            (_, mut o_emu, mut o_view, mut o_pp, _),
            (_, mut s_emu, mut s_view, mut s_pp, _),
        ],
    ) = views.get_many_mut([origin, spare])
    else {
        return;
    };

    s_view.index = o_view.index;
    o_view.index = CROSSFADE_INDEX;
    s_pp.alpha = 1.0;
    o_pp.alpha = 0.0;
    o_emu.is_crossfade = true;
    s_emu.is_crossfade = false;
    o_emu.core = None;

    debug!("Cross fade emulator took over view {}", s_view.index);
    state.spare = Some(origin);
    state.origin = None;
    state.loaded_at = None;
}

/// Move a pending advance off the view that asked for it and onto the spare, so
/// `handle_loading` drives the load there instead.
fn hijack_load(mut state: ResMut<CrossFade>, mut views: Views) {
    // An advance asked for while the fade is still running ends it early; the
    // request moves to the view that just took over and is hijacked below.
    if state.loaded_at.is_some() {
        let (Some(origin), Some(takes_over)) = (state.origin, state.spare) else {
            return;
        };
        let Ok((_, mut emu, ..)) = views.get_mut(origin) else {
            return;
        };
        let advance = (emu.run_next, emu.run_prev);
        if !(advance.0 || advance.1) {
            return;
        }
        (emu.run_next, emu.run_prev) = (false, false);
        swap_roles(&mut state, &mut views);
        if let Ok((_, mut emu, ..)) = views.get_mut(takes_over) {
            (emu.run_next, emu.run_prev) = advance;
        }
    }

    let Some(spare) = state.spare else {
        return;
    };
    let Ok((_, emu, ..)) = views.get(spare) else {
        return;
    };
    if emu.is_loading() || emu.run_next || emu.run_prev {
        return;
    }

    let Some((origin, advance, index, cell)) = views
        .iter()
        .find(|(_, emu, ..)| !emu.is_crossfade && (emu.run_next || emu.run_prev))
        .map(|(e, emu, view, _, cell)| {
            (e, (emu.run_next, emu.run_prev), view.index, cell.copied())
        })
    else {
        return;
    };

    if let Ok((_, mut emu, ..)) = views.get_mut(origin) {
        emu.run_next = false;
        emu.run_prev = false;
    }
    let Ok((_, mut emu, _, _, spare_cell)) = views.get_mut(spare) else {
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

/// The spare finished loading: start the clock the delay and the fade run on.
fn start_fade(
    mut finished: MessageReader<LoadFinished>,
    mut state: ResMut<CrossFade>,
    time: Res<Time>,
) {
    for LoadFinished(entity) in finished.read() {
        if state.spare == Some(*entity) && state.origin.is_some() {
            state.loaded_at = Some(time.elapsed_secs_f64());
        }
    }
}

/// Hold the spare hidden for `DELAY_TIME`, fade it up over `FADE_TIME`, then
/// hand it the view.
fn run_fade(mut state: ResMut<CrossFade>, mut views: Views, time: Res<Time>) {
    let (Some(loaded_at), Some(spare)) = (state.loaded_at, state.spare) else {
        return;
    };
    let fading = (time.elapsed_secs_f64() - loaded_at) as f32 - DELAY_TIME;
    if fading < 0.0 {
        return;
    }
    if fading >= FADE_TIME {
        swap_roles(&mut state, &mut views);
        return;
    }
    if let Ok((_, _, _, mut pp, _)) = views.get_mut(spare) {
        pp.alpha = fading / FADE_TIME;
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
                    start_fade.after(crate::frontend::handle_loading),
                    run_fade.after(start_fade),
                ),
            );
    }
}
