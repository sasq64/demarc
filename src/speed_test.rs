//! Benchmark driver for `--speed-test`.

use bevy::prelude::*;

use crate::AppSettings;
use crate::emulator::Emulator;

/// Tracks the `--speed-test` measurement window across frames.
#[derive(Default)]
struct SpeedTestState {
    /// App time of the first tick, used for the boot-time-grace fallback.
    first_tick: Option<f64>,
    /// App time at which frames first started flowing, marking the start of the
    /// warm-up period.
    frames_started: Option<f64>,
    /// (start time, baseline frame count) once the measurement window opens.
    window_start: Option<(f64, u64)>,
    /// Set once the result has been reported, to avoid printing/exiting twice.
    done: bool,
}

/// Wall-clock seconds to keep running after frames first appear before opening
/// the measurement window, so one-time setup (core boot, render-pipeline/shader
/// compilation, first-frame allocations) is excluded from the measured rate.
const SPEED_TEST_WARMUP: f64 = 1.0;
/// Length of the measured window, in wall-clock seconds.
const SPEED_TEST_MEASURE: f64 = 2.0;

/// Benchmark driver for `--speed-test`: once the emulator is stepping frames,
/// warms up briefly to get past all one-time setup, then measures a fixed
/// window of unthrottled emulation, prints the frame count, and requests app
/// exit.
fn speed_test_monitor(
    settings: Res<AppSettings>,
    emus: Query<&Emulator>,
    time: Res<Time>,
    mut state: Local<SpeedTestState>,
    mut exit: MessageWriter<AppExit>,
) {
    if !settings.speed_test || state.done {
        return;
    }
    let now = time.elapsed_secs_f64();
    let first = *state.first_tick.get_or_insert(now);
    let total: u64 = emus
        .iter()
        .filter_map(|e| e.core.as_ref())
        .map(|c| c.frames_stepped())
        .sum();

    // Phase 1: wait for the core to actually start stepping frames (excludes
    // core creation and boot-to-first-frame), or fall back after a grace period
    // for cores that never report a frame count.
    let Some(warmup_start) = state.frames_started else {
        if total > 0 || now - first > 10.0 {
            state.frames_started = Some(now);
        }
        return;
    };

    // Phase 2: warm up for a bit so render-pipeline/shader compilation and other
    // one-time setup finish before the clock starts.
    let (start_t, base) = match state.window_start {
        Some(v) => v,
        None => {
            if now - warmup_start >= SPEED_TEST_WARMUP {
                state.window_start = Some((now, total));
            }
            return;
        }
    };

    // Phase 3: measure the fixed window.
    let secs = now - start_t;
    if secs >= SPEED_TEST_MEASURE {
        let stepped = total - base;
        println!(
            "Speed test: {stepped} frames in {secs:.2}s = {:.1} fps",
            stepped as f64 / secs
        );
        state.done = true;
        exit.write(AppExit::Success);
    }
}

pub struct SpeedTestPlugin;

impl Plugin for SpeedTestPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, speed_test_monitor);
    }
}
