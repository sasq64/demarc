//! Runs a libretro core on a thread of its own, so the frontend never blocks on
//! emulation. Commands go out over a channel, finished frames come back.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use tracing::{error, trace};

use crate::backend::{Backend, ViewFocus};
use crate::pixels::scan_frame;

use super::RetroCoreDirect;

/// Stack for the thread a core runs on. See the `stack_size` call in
/// [`RetroCoreThreaded::new`] for why the default is not enough.
const WORKER_STACK_SIZE: usize = 32 * 1024 * 1024;

/// How long a key scheduled by [`RetroCmd::SendKeys`] stays down before the
/// matching release is sent. Long enough for any core to notice the press.
const KEY_HOLD_FRAMES: u64 = 2;

/// Commands the main thread sends to the worker that owns the `RetroCore`.
enum RetroCmd {
    Reset,
    PressKey {
        code: u32,
        down: bool,
        mods: u16,
    },
    AddMouseMotion {
        dx: f32,
        dy: f32,
    },
    SetMouseButtons {
        left: bool,
        right: bool,
        middle: bool,
    },
    SetJoypad {
        port: u32,
        id: u32,
        down: bool,
    },
    SetDisk {
        no: u32,
    },
    Unload,
    Skip {
        frames: u32,
    },
    SetFocus {
        focus: ViewFocus,
    },
    /// `(frame, keycode)` pairs, where the frame is relative to whenever the
    /// worker picks the command up — `0` meaning the next stepped frame.
    SendKeys {
        time_code_list: Vec<(u32, u32)>,
    },
}

/// A single stepped frame's worth of data, pushed from the worker to main thread
#[derive(Default)]
struct RetroUpdate {
    width: usize,
    height: usize,
    frame: Vec<u32>,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
    frame_hash: u64,
}

pub struct RetroCoreThreaded {
    cmd_tx: mpsc::Sender<RetroCmd>,
    // Wrapped in a `Mutex` purely so the type is `Sync`: `mpsc::Receiver` is
    // `Send` but not `Sync`, and Bevy requires components to be `Sync`. All
    // access is through `&mut self` (`run`/`Drop`), so `get_mut` is used and
    // the lock is never actually contended.
    update_rx: Mutex<mpsc::Receiver<RetroUpdate>>,
    handle: Option<thread::JoinHandle<()>>,
    frame: Vec<u32>,
    frame_hash: u64,
    last_hash: u64,
    audio_sum: i32,
    last_sum: i32,
    frame_width: usize,
    frame_height: usize,
    audio: Vec<i16>,
    aspect_ratio: f32,
    aspect_tweak: f32,
    sample_rate: f64,
    fps: f64,
    disk_count: u32,
    /// Emulated frames stepped by the worker so far (shared with the worker
    /// thread); read by `--speed-test`.
    frames: Arc<AtomicU64>,
}

struct SetupResult {
    fps: f64,
    width: usize,
    height: usize,
    disks: u32,
}

impl RetroCoreThreaded {
    pub fn new(
        core_path: &Path,
        system_dir: &Path,
        game: Option<&Path>,
        meta: HashMap<String, String>,
        speed_test: bool,
    ) -> Result<Self> {
        let core_path = core_path.to_path_buf();
        let system_dir = system_dir.to_path_buf();
        let game = game.map(|g| g.to_path_buf());

        let is_atari = core_path
            .file_name()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default()
            .contains("hatari");

        // TODO: Why is this necessary
        let aspect_tweak = if is_atari { 1.13 } else { 1.0 };

        let mut latency = 3;
        if let Some(l) = meta.get("latency") {
            latency = l.parse().unwrap_or(3);
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<RetroCmd>();
        let (update_tx, update_rx) = mpsc::sync_channel::<RetroUpdate>(latency);
        let (setup_tx, setup_rx) = mpsc::channel::<Result<SetupResult, String>>();

        let frames = Arc::new(AtomicU64::new(0));
        let worker_frames = Arc::clone(&frames);
        let handle = thread::Builder::new()
            .name("retro-emu".into())
            // Well above the 2 MiB default. Cores recurse deeply on this thread —
            // a dynarec or shader compiler can overflow the default and take the
            // process down with a SIGSEGV that looks nothing like a stack overflow.
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                let mut core =
                    match RetroCoreDirect::new(&core_path, &system_dir, game.as_deref(), meta) {
                        Ok(mut core) => {
                            let _ = setup_tx.send(Ok(SetupResult {
                                fps: core.fps(),
                                width: core.get_frame_size().0,
                                height: core.get_frame_size().1,
                                disks: core.get_number_of_disks(),
                            }));
                            core
                        }
                        Err(e) => {
                            let _ = setup_tx.send(Err(e.to_string()));
                            return;
                        }
                    };
                worker_loop(&mut core, &cmd_rx, &update_tx, &worker_frames, speed_test);
                // `core` is dropped here, running retro_deinit on this thread.
            })?;

        match setup_rx.recv() {
            Ok(Ok(SetupResult {
                fps,
                width,
                height,
                disks,
            })) => Ok(Self {
                cmd_tx,
                update_rx: Mutex::new(update_rx),
                handle: Some(handle),
                frame: Vec::new(),
                frame_hash: 0,
                last_hash: 0,
                audio_sum: 0,
                last_sum: 0,
                frame_width: width,
                frame_height: height,
                audio: Vec::new(),
                aspect_ratio: 0.0,
                aspect_tweak,
                sample_rate: 0.0,
                fps,
                disk_count: disks,
                frames,
            }),
            Ok(Err(e)) => {
                let _ = handle.join();
                Err(anyhow!("failed to create core: {e}"))
            }
            Err(_) => {
                let _ = handle.join();
                Err(anyhow!("retro worker thread exited before setup"))
            }
        }
    }
}

/// Worker-thread main loop
fn worker_loop(
    core: &mut RetroCoreDirect,
    cmd_rx: &mpsc::Receiver<RetroCmd>,
    update_tx: &mpsc::SyncSender<RetroUpdate>,
    frames: &AtomicU64,
    speed_test: bool,
) {
    // Keys scheduled by `RetroCmd::SendKeys`, as (frame to fire on, keycode,
    // pressed). Frames are absolute counts of `frames`, so nothing can be
    // scheduled into the past.
    let mut key_queue: Vec<(u64, u32, bool)> = Vec::new();
    loop {
        let frame = frames.load(Ordering::Relaxed);

        // Drain all pending commands without blocking.
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if apply_cmd(core, cmd, &mut key_queue, frame) {
                        return; // Unload
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        // Play back every scheduled key that is due this frame.
        if !key_queue.is_empty() {
            key_queue.retain(|&(at, code, down)| {
                if at > frame {
                    return true;
                }
                core.press_key(code, down, 0);
                false
            });
        }

        if core.visible {
            core.run();
            // Count every emulated frame the core steps, including skipped ones.
            frames.fetch_add(1, Ordering::Relaxed);
            if core.skip_frames > 0 {
                core.skip_frames -= 1;
                // Throw away the audio the core just produced.
                core.with_audio(|_| {});
                continue;
            }

            let (width, height) = core.get_frame_size();
            let mut frame = Vec::new();
            core.with_frame(|_, _, fr| frame.extend_from_slice(fr));

            let hash = scan_frame(&frame);

            let mut audio = Vec::new();
            core.with_audio(|s| audio.extend_from_slice(s));

            let update = RetroUpdate {
                width,
                height,
                frame,
                audio,
                aspect_ratio: core.aspect_ratio(),
                sample_rate: core.sample_rate(),
                fps: core.fps(),
                frame_hash: hash,
            };
            if speed_test {
                // Benchmark: never block on the consumer. Hand off the latest frame
                // if there's room, otherwise drop it and keep emulating flat-out so
                // throughput reflects the core, not the (vsync-limited) main loop.
                match update_tx.try_send(update) {
                    Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                    Err(mpsc::TrySendError::Disconnected(_)) => return,
                }
            } else if update_tx.send(update).is_err() {
                return; // main side gone
            }
        }
    }
}

/// Apply one command to the core. Returns `true` if the worker should stop.
///
/// `key_queue` is the worker's scheduled-key list and `frame` its current frame
/// counter; `SendKeys` appends to the former relative to the latter.
fn apply_cmd(
    core: &mut RetroCoreDirect,
    cmd: RetroCmd,
    key_queue: &mut Vec<(u64, u32, bool)>,
    frame: u64,
) -> bool {
    match cmd {
        RetroCmd::Reset => core.reset(),
        RetroCmd::PressKey { code, down, mods } => core.press_key(code, down, mods),
        RetroCmd::AddMouseMotion { dx, dy } => core.add_mouse_motion(dx, dy),
        RetroCmd::SetMouseButtons {
            left,
            right,
            middle,
        } => core.set_mouse_buttons(left, right, middle),
        RetroCmd::SetJoypad { port, id, down } => core.set_joypad(port, id, down),
        RetroCmd::SetDisk { no } => {
            core.set_disk(no);
        }
        RetroCmd::SetFocus { focus } => {
            core.focus(focus);
        }
        RetroCmd::Unload => {
            core.unload();
            return true;
        }
        RetroCmd::Skip { frames } => core.skip_frames = frames,
        RetroCmd::SendKeys { time_code_list } => {
            for (at, code) in time_code_list {
                // Relative to now, so a core's startup keys can't land in the past.
                let at = frame + at as u64;
                key_queue.push((at, code, true));
                key_queue.push((at + KEY_HOLD_FRAMES, code, false));
            }
        }
    }
    false
}

impl Backend for RetroCoreThreaded {
    fn run(&mut self) -> bool {
        if let Ok(update) = self.update_rx.get_mut().unwrap().try_recv() {
            // if update.frame.is_empty() && update.audio.is_empty() {
            //     info!("GOT 0 UPDATE");
            //     self.audio.clear();
            //     return false;
            // }
            self.frame = update.frame;
            self.last_hash = self.frame_hash;
            self.frame_hash = update.frame_hash;
            self.frame_width = update.width;
            self.frame_height = update.height;
            self.last_sum = self.audio_sum;
            self.audio_sum = update.audio.iter().map(|a| (*a as i32).abs()).sum();
            self.audio.extend_from_slice(&update.audio);
            self.aspect_ratio = update.aspect_ratio;
            self.sample_rate = update.sample_rate;
            self.fps = update.fps;
            true
        } else {
            trace!("Starving");
            false
        }
    }

    fn focus(&mut self, focus: ViewFocus) {
        let _ = self.cmd_tx.send(RetroCmd::SetFocus { focus });
    }

    fn send_keys(&mut self, keys: &[(u32, u32)]) {
        let _ = self.cmd_tx.send(RetroCmd::SendKeys {
            time_code_list: keys.to_vec(),
        });
    }

    fn is_idle(&self) -> bool {
        self.last_hash == self.frame_hash && self.audio_sum.abs() < 1000
    }

    fn get_number_of_disks(&mut self) -> u32 {
        self.disk_count
    }
    fn reset(&mut self) {
        let _ = self.cmd_tx.send(RetroCmd::Reset);
    }
    fn set_disk(&mut self, no: u32) {
        if self.cmd_tx.send(RetroCmd::SetDisk { no }).is_err() {}
    }
    fn press_key(&mut self, code: u32, down: bool, mods: u16) {
        let _ = self.cmd_tx.send(RetroCmd::PressKey { code, down, mods });
    }
    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        let _ = self.cmd_tx.send(RetroCmd::AddMouseMotion { dx, dy });
    }
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        let _ = self.cmd_tx.send(RetroCmd::SetMouseButtons {
            left,
            right,
            middle,
        });
    }
    fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        let _ = self.cmd_tx.send(RetroCmd::SetJoypad { port, id, down });
    }
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(self.frame_width, self.frame_height, &self.frame);
    }
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.audio);
        self.audio.clear();
    }
    fn get_frame_size(&self) -> (usize, usize) {
        (self.frame_width, self.frame_height)
    }
    fn aspect_ratio(&self) -> f32 {
        self.aspect_ratio * self.aspect_tweak
    }
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    fn fps(&self) -> f64 {
        self.fps
    }
    // fn unload(&mut self) {
    //     let _ = self.cmd_tx.send(RetroCmd::Unload);
    // }

    fn skip_frames(&mut self, frames: u32) {
        let _ = self.cmd_tx.send(RetroCmd::Skip { frames });
    }
    fn frames_stepped(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    fn frame_hash(&self) -> u64 {
        self.frame_hash
    }
}

/// How long to wait for the worker to shut the core down before giving up on
/// it. Generous: it covers a core still finishing the frame it is in the
/// middle of, and every core here is done long inside it. What it rules out is
/// the other case — a core wedged in its own shutdown taking the whole
/// application down with it, since this runs on the main thread while the user
/// is trying to quit.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

impl Drop for RetroCoreThreaded {
    fn drop(&mut self) {
        // Ask the worker to stop. It only checks for Unload at the top of its
        // loop, but with a bounded update channel it may currently be parked in
        // a full `update_tx.send()`. Keep draining the channel so that send
        // completes and the worker can loop back, observe the Unload, and
        // return — otherwise the join below would deadlock. `recv` returns Err
        // once the worker has dropped its SyncSender, which it does after the
        // core has been unloaded, so a disconnect means the join is free.
        let _ = self.cmd_tx.send(RetroCmd::Unload);
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        let rx = self.update_rx.get_mut().unwrap();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(_) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // The worker is stuck inside the core — the one place this
                    // has been seen is a core whose emulation thread never
                    // stops. Leave it running rather than joining it: a hung
                    // worker would otherwise hang the caller, which is the main
                    // thread on its way out of the process.
                    error!("Core did not shut down in {SHUTDOWN_TIMEOUT:?}, leaving it running");
                    return;
                }
            }
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Compile-time guarantee that the direct core can be moved onto the worker
/// thread, and that the threaded handle is `Send + Sync` so it can live inside
/// a Bevy component (the `Emulator`).
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send::<RetroCoreDirect>();
        _assert_send_sync::<RetroCoreThreaded>();
    }
};
