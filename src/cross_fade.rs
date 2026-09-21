//! Loading the next release into a spare, off-screen emulator and fading it in
//! over the one it replaces.
//!
//! A load drops the running core before it builds the new one, so an
//! emulator that loads cannot keep showing what it was showing. With
//! `--cross-fade` an extra emulator entity (the *spare*) is spawned and every
//! advance is diverted into it. Once the load lands the spare keeps running off
//! screen for [`DELAY_TIME`], fades up over [`FADE_TIME`], and then the two
//! entities trade roles: the spare takes the origin's view index, the origin
//! becomes the new spare.
//!
//! `--dj-mode` ([`crate::dj`]) gives the spare a window of its own and leaves
//! the fade to [`Cmd::StartOther`]: nothing reaches the main window until it is
//! asked for, and a load while one is waiting simply replaces it.
//!
//! There is one spare, so under `--grid` only the first emulator asking to
//! advance in a frame is diverted; while that load runs the others load into
//! themselves as they always did. Cross fade is aimed at the single view case.

use bevy::prelude::*;

use crate::commands::{Cmd, CmdMessage};
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
const FADE_TIME: f32 = 4.0;

/// One emulator finished a load this frame.
#[derive(Message)]
pub struct LoadFinished(pub Entity);

#[derive(Resource, Default)]
struct CrossFade {
    spare: Option<Entity>,
    /// The view the load in flight was taken from.
    origin: Option<Entity>,
    /// When the spare's load finished; `DELAY_TIME` runs from here. `Some`
    /// exactly while the fade is pending or running.
    loaded_at: Option<f64>,
    /// When the fade itself started — the delay was over and the picture had
    /// passed `--cross-fade-activity`. `FADE_TIME` runs from here.
    fade_at: Option<f64>,
    /// [`Cmd::StartOther`] asked for the fade: it starts as soon as the load
    /// lands, without the delay or the activity check. Under `--dj-mode` it is
    /// the only thing that starts one.
    armed: bool,
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
    // Its sink runs from the start (see `run_frontend`); this is what keeps it
    // silent until a fade ramps it up.
    emu.set_volume(0.0);
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
    s_emu.set_volume(1.0);
    o_emu.set_volume(0.0);
    o_emu.is_crossfade = true;
    s_emu.is_crossfade = false;
    // Off the main thread: tearing a core down joins its worker thread, which
    // landed as a stutter on the frame the fade ended.
    o_emu.drop_core_async();

    debug!("Cross fade emulator took over view {}", s_view.index);
    state.spare = Some(origin);
    state.origin = None;
    state.loaded_at = None;
    state.fade_at = None;
    state.armed = false;
}

/// Move a pending advance off the view that asked for it and onto the spare, so
/// `handle_loading` drives the load there instead.
fn hijack_load(mut state: ResMut<CrossFade>, mut views: Views, args: Res<Args>) {
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
        // In DJ mode a waiting load has not reached the screen at all, so the
        // new one simply replaces it — the advance is left where it is and
        // hijacked below.
        if args.dj_mode && state.fade_at.is_none() {
            state.loaded_at = None;
        } else {
            (emu.run_next, emu.run_prev) = (false, false);
            swap_roles(&mut state, &mut views);
            if let Ok((_, mut emu, ..)) = views.get_mut(takes_over) {
                (emu.run_next, emu.run_prev) = advance;
            }
        }
    }

    let Some(spare) = state.spare else {
        return;
    };
    let Ok((_, emu, ..)) = views.get(spare) else {
        return;
    };
    if emu.is_loading() || emu.run_next || emu.run_prev {
        // The cue is busy. In DJ mode the request is dropped rather than left
        // where it is: `handle_loading` would otherwise act on it where it
        // stands and load over the view that is on screen.
        if args.dj_mode {
            for (_, mut emu, ..) in views.iter_mut() {
                if !emu.is_crossfade {
                    (emu.run_next, emu.run_prev) = (false, false);
                }
            }
        }
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
    // A fade asked for before this load was started is not a fade of it.
    state.armed = false;
}

/// [`Cmd::StartOther`]: bring the cue in by hand.
fn arm_fade(mut cmds: MessageReader<CmdMessage>, mut state: ResMut<CrossFade>) {
    if cmds.read().any(|cmd| cmd.0 == Cmd::StartOther) {
        state.armed = true;
    }
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
            state.fade_at = None;
        }
    }
}

/// Hold the spare hidden for `DELAY_TIME` — and, with
/// `--cross-fade-activity`, until its picture is moving enough — fade it up
/// over `FADE_TIME`, then hand it the view.
fn run_fade(mut state: ResMut<CrossFade>, mut views: Views, time: Res<Time>, args: Res<Args>) {
    let (Some(loaded_at), Some(spare)) = (state.loaded_at, state.spare) else {
        return;
    };
    let now = time.elapsed_secs_f64();
    let started_at = match state.fade_at {
        Some(started_at) => started_at,
        // Asked for by hand: no delay, no activity check.
        None if state.armed => {
            state.fade_at = Some(now);
            now
        }
        // In DJ mode that is the only way a fade starts.
        None if args.dj_mode => return,
        None => {
            if ((now - loaded_at) as f32) < DELAY_TIME {
                return;
            }
            if let Some(threshold) = args.cross_fade_activity {
                let activity = views
                    .get(spare)
                    .ok()
                    .and_then(|(_, emu, ..)| emu.core.as_ref().map(|c| c.screen_activity()))
                    .unwrap_or(1.0);
                if activity < threshold {
                    return;
                }
            }
            state.fade_at = Some(now);
            now
        }
    };
    let fading = (now - started_at) as f32;
    if fading >= FADE_TIME {
        swap_roles(&mut state, &mut views);
        return;
    }
    let level = fading / FADE_TIME;
    if let Ok((_, emu, _, mut pp, _)) = views.get_mut(spare) {
        pp.alpha = level;
        // Equal power, so the pair keeps a steady loudness across the fade
        // where a linear one would dip in the middle.
        emu.set_volume(level.sqrt());
    }
    if let Some(origin) = state.origin
        && let Ok((_, emu, ..)) = views.get(origin)
    {
        emu.set_volume((1.0 - level).sqrt());
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
                    arm_fade.after(crate::commands::handle_cmd),
                    run_fade.after(start_fade).after(arm_fade),
                ),
            );
    }
}
