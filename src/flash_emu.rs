//! Flash (SWF) backend built on the [Ruffle](https://ruffle.rs) Flash emulator.
//!
//! [`FlashEmu`] implements [`RetroEmu`] so a `.swf` slots into the same frontend
//! plumbing as a libretro core: RGBA frames via `with_frame`, interleaved i16
//! stereo via `with_audio`, mouse/keyboard input, and the standard timing
//! getters.
//!
//! Ruffle renders through `wgpu` (it has no CPU rasterizer) and its `Player`
//! carries a thread-local gc-arena, so — mirroring [`RetroCoreThreaded`] — the
//! actual emulator lives on a dedicated worker thread that owns the `Player`, an
//! offscreen `wgpu` instance, and the audio mixer. `FlashEmu` itself holds only
//! channels plus a cached frame/audio buffer, which makes it trivially
//! `Send + Sync` (required for the Bevy `Emulator` component) and keeps all wgpu
//! work off Bevy's main thread.
//!
//! [`RetroCoreThreaded`]: crate::retro_emu::RetroCoreThreaded

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::thread::{self, JoinHandle};

use anyhow::{Result, anyhow, bail};

use ruffle_core::backend::audio::{
    AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
use ruffle_core::backend::navigator::{NullExecutor, NullNavigatorBackend};
use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton as RuffleMouseButton, NamedKey,
    PhysicalKey, PlayerEvent,
};
use ruffle_core::tag_utils::movie_from_path;
use ruffle_core::{FloatDuration, PlayerBuilder, impl_audio_mixer_backend};
use ruffle_render_wgpu::backend::{
    WgpuRenderBackend, create_wgpu_instance, request_adapter_and_device,
};
use ruffle_render_wgpu::descriptors::Descriptors;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;

use std::sync::Arc;

use crate::retro_emu::RetroEmu;

/// Audio output rate handed to the frontend. Matches the other cores so the
/// existing [`crate::audio::AudioResampler`] converts to the device rate.
const SAMPLE_RATE: u32 = 44100;
/// Channel depth for frame updates; a few frames of slack, like the libretro
/// worker ([`crate::retro_emu::RetroCoreThreaded`]). Blocking sends provide
/// backpressure so the worker runs at the frontend's consumption rate.
const UPDATE_QUEUE: usize = 3;

/// Commands sent from [`FlashEmu`] (main thread) to the Ruffle worker.
enum FlashCmd {
    PressKey {
        code: u32,
        down: bool,
    },
    MouseMotion {
        dx: f32,
        dy: f32,
    },
    /// Absolute pointer position in normalized frame coords (`0.0..=1.0`).
    MousePosition {
        x: f32,
        y: f32,
    },
    MouseButtons {
        left: bool,
        right: bool,
        middle: bool,
    },
    Reset,
    SavePng(PathBuf),
    Skip(u32),
    Unload,
}

/// One presented frame produced by the worker.
struct FlashUpdate {
    width: usize,
    height: usize,
    /// RGBA8, tightly packed, alpha forced opaque.
    frame: Vec<u8>,
    /// Interleaved stereo i16 for this frame (`SAMPLE_RATE / fps` samples).
    audio: Vec<i16>,
    /// Live movie frame rate (ActionScript can change it at runtime).
    fps: f64,
}

/// Result of the worker's one-time setup, awaited synchronously by
/// [`FlashEmu::new`] so SWF/GPU load errors surface at load time.
enum SetupResult {
    Ok { width: u32, height: u32, fps: f64 },
    Err(String),
}

/// A Ruffle [`AudioBackend`] that is just a thin owner of an [`AudioMixer`].
/// Sound registration/playback is delegated to the mixer by the macro; the
/// mixed output itself is pulled through an [`AudioMixer::proxy`] kept in the
/// worker (exactly how Ruffle's cpal backend works).
struct FlashAudio {
    mixer: AudioMixer,
}

impl AudioBackend for FlashAudio {
    impl_audio_mixer_backend!(mixer);

    // Playback is pull-driven by the frontend, so these are no-ops.
    fn play(&mut self) {}
    fn pause(&mut self) {}
}

/// `Send + Sync` handle to a Ruffle player running on its own thread.
pub struct FlashEmu {
    cmd_tx: Sender<FlashCmd>,
    // `Receiver` is `Send` but not `Sync`; the `Mutex` makes `FlashEmu: Sync`
    // (required for the Bevy component). `run`/`Drop` take `&mut self`, so this
    // is always accessed via `get_mut` with no locking cost.
    update_rx: Mutex<Receiver<FlashUpdate>>,
    worker: Option<JoinHandle<()>>,

    // Latest presented state, refreshed by `run`.
    width: usize,
    height: usize,
    frame: Vec<u8>,
    /// Accumulates across `run` calls; drained by `with_audio`.
    audio: Vec<i16>,
    fps: f64,
}

impl FlashEmu {
    pub fn new(game: &Path, _tags: std::collections::HashMap<String, String>) -> Result<Self> {
        let game = game.to_path_buf();
        let (cmd_tx, cmd_rx) = channel::<FlashCmd>();
        let (update_tx, update_rx) = sync_channel::<FlashUpdate>(UPDATE_QUEUE);
        let (setup_tx, setup_rx) = channel::<SetupResult>();

        let thread_game = game.clone();
        let worker = thread::Builder::new()
            .name("flash-emu".into())
            .spawn(move || worker_loop(&thread_game, cmd_rx, update_tx, setup_tx))?;

        match setup_rx.recv() {
            Ok(SetupResult::Ok { width, height, fps }) => Ok(Self {
                cmd_tx,
                update_rx: Mutex::new(update_rx),
                worker: Some(worker),
                width: width as usize,
                height: height as usize,
                frame: vec![0u8; width as usize * height as usize * 4],
                audio: Vec::new(),
                fps,
            }),
            Ok(SetupResult::Err(e)) => {
                let _ = worker.join();
                bail!("Failed to load SWF '{}': {e}", game.display());
            }
            Err(_) => {
                let _ = worker.join();
                bail!("Flash worker exited before setup for '{}'", game.display());
            }
        }
    }
}

impl RetroEmu for FlashEmu {
    fn run(&mut self) -> bool {
        // Consume exactly one frame per call, like `RetroCoreThreaded::run`: the
        // frontend (`Emulator::run`) paces `run()` at the movie's fps and assumes
        // each call carries one frame's worth of audio. Draining the whole channel
        // here instead would push several frames of audio per paced call (flooding
        // the sink past `AUDIO_BUF_MAX` -> continuous "Dropping frame") and would
        // keep the bounded channel from ever filling, defeating the blocking-send
        // backpressure that throttles the worker to real time.
        if let Ok(update) = self.update_rx.get_mut().unwrap().try_recv() {
            self.width = update.width;
            self.height = update.height;
            self.frame = update.frame;
            self.audio.extend_from_slice(&update.audio);
            self.fps = update.fps;
        }
        true
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u8])) {
        f(self.width, self.height, &self.frame);
    }

    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.audio);
        self.audio.clear();
    }

    fn get_frame_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn aspect_ratio(&self) -> f32 {
        if self.height == 0 {
            0.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    fn sample_rate(&self) -> f64 {
        SAMPLE_RATE as f64
    }

    fn fps(&self) -> f64 {
        self.fps
    }

    fn press_key(&mut self, code: u32, down: bool, _mods: u16) {
        let _ = self.cmd_tx.send(FlashCmd::PressKey { code, down });
    }

    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        let _ = self.cmd_tx.send(FlashCmd::MouseMotion { dx, dy });
    }

    fn set_mouse_position(&mut self, x: f32, y: f32) {
        let _ = self.cmd_tx.send(FlashCmd::MousePosition { x, y });
    }

    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        let _ = self.cmd_tx.send(FlashCmd::MouseButtons {
            left,
            right,
            middle,
        });
    }

    // SWFs have no disks or joypads.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&self) -> u32 {
        0
    }
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}

    fn reset(&mut self) {
        let _ = self.cmd_tx.send(FlashCmd::Reset);
    }

    fn save_png(&self, path: &Path) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.cmd_tx
            .send(FlashCmd::SavePng(path.to_path_buf()))
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
        Ok(())
    }

    fn skip_frames(&mut self, frames: u32) {
        let _ = self.cmd_tx.send(FlashCmd::Skip(frames));
    }

    fn unload(&mut self) {
        let _ = self.cmd_tx.send(FlashCmd::Unload);
    }
}

impl Drop for FlashEmu {
    fn drop(&mut self) {
        let _ = self.cmd_tx.send(FlashCmd::Unload);
        // Drain any queued frame so a blocked worker send can unblock, then join.
        if let Ok(rx) = self.update_rx.get_mut() {
            while rx.try_recv().is_ok() {}
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Owns the Ruffle player and drives it in real time. All Ruffle / wgpu state
/// stays on this thread.
fn worker_loop(
    game: &Path,
    cmd_rx: Receiver<FlashCmd>,
    update_tx: SyncSender<FlashUpdate>,
    setup_tx: Sender<SetupResult>,
) {
    let setup = build_player(game);
    let (player, proxy, mut executor, width, height, fps) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = setup_tx.send(SetupResult::Err(e.to_string()));
            return;
        }
    };
    let _ = setup_tx.send(SetupResult::Ok { width, height, fps });

    // Absolute cursor position in stage pixels; Ruffle wants absolute coords but
    // the RetroEmu trait only offers relative motion.
    let mut cursor = (width as f64 / 2.0, height as f64 / 2.0);
    let mut buttons = (false, false, false);
    // Fractional carry so the per-frame audio chunk averages exactly
    // `SAMPLE_RATE / fps` samples even when that isn't a whole number.
    let mut audio_carry = 0.0f64;

    loop {
        // Apply all pending commands.
        let mut skip = 0u32;
        loop {
            match cmd_rx.try_recv() {
                Ok(FlashCmd::Unload) => return,
                Ok(cmd) => apply_cmd(
                    &player,
                    cmd,
                    width,
                    height,
                    &mut cursor,
                    &mut buttons,
                    &mut skip,
                ),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        // Produce exactly one movie frame per iteration and hand it to the
        // channel, mirroring the libretro worker
        // ([`crate::retro_emu::RetroCoreThreaded`]): the worker does no timing of
        // its own. The blocking send below makes it run at the frontend's
        // consumption rate, and the frontend's `Emulator` layer owns pacing and
        // audio-rate matching just as it does for a libretro core.
        //
        // "One frame" is `tick(1/fps)`, not a bare `run_frame()`: alongside the
        // timeline it advances `flash.utils` timers, `NetStream`/streamed-sound
        // playback, and the audio backend — machinery timer-driven AS3 content
        // (e.g. this Flex/Away3D demo, whose menu and music are timer/stream
        // driven) needs to progress at all — and it stream-preloads incrementally
        // so big movies present as they load. Passing the movie's own frame
        // duration advances exactly one frame; it is a per-frame quantum, not
        // wall-clock pacing.
        let live_fps = {
            let mut p = player.lock().unwrap();
            let frame_dur = FloatDuration::from_secs(1.0 / p.frame_rate().max(1.0));
            for _ in 0..=skip {
                p.tick(frame_dur);
            }
            p.render();
            p.frame_rate().max(1.0)
        };
        // Pump the navigator's executor so pending external fetches (e.g. the
        // relative `.mp3` load) make progress; results are delivered on the next
        // `tick`.
        executor.run();

        let frame = capture_frame(&player)
            .unwrap_or_else(|| vec![0u8; width as usize * height as usize * 4]);

        // One frame's worth of audio, frame-locked to video like a libretro
        // core's per-`run` audio batch.
        audio_carry += SAMPLE_RATE as f64 * (skip + 1) as f64 / live_fps;
        let n_frames = audio_carry.floor() as usize;
        audio_carry -= n_frames as f64;
        let mut audio = vec![0i16; n_frames * 2];
        proxy.mix::<i16>(&mut audio);

        // Blocking send paces the worker to the frontend's consumption.
        if update_tx
            .send(FlashUpdate {
                width: width as usize,
                height: height as usize,
                frame,
                audio,
                fps: live_fps,
            })
            .is_err()
        {
            return;
        }
    }
}

type PlayerHandle = Arc<std::sync::Mutex<ruffle_core::Player>>;

/// Build the wgpu instance, load the movie, and construct the Ruffle player.
/// Returns the player, an audio pull-proxy, and the movie geometry/fps.
fn build_player(
    game: &Path,
) -> Result<(
    PlayerHandle,
    ruffle_core::backend::audio::AudioMixerProxy,
    NullExecutor,
    u32,
    u32,
    f64,
)> {
    let instance = create_wgpu_instance(wgpu::Backends::all(), wgpu::BackendOptions::default());
    let (adapter, device, queue) = pollster::block_on(request_adapter_and_device(
        wgpu::Backends::all(),
        &instance,
        None,
        wgpu::PowerPreference::HighPerformance,
    ))
    .map_err(|e| anyhow!("no usable GPU adapter for Flash rendering: {e} (a software Vulkan/GL driver such as lavapipe works)"))?;
    let descriptors = Arc::new(Descriptors::new(instance, adapter, device, queue));

    let movie = movie_from_path(game, None).map_err(|e| anyhow!(e.to_string()))?;
    let width = movie.width().to_pixels().max(1.0) as u32;
    let height = movie.height().to_pixels().max(1.0) as u32;
    let fps = movie.frame_rate().to_f64().max(1.0);

    let target = TextureTarget::new(&descriptors.device, (width, height))
        .map_err(|e| anyhow!(e.to_string()))?;
    let renderer =
        WgpuRenderBackend::new(descriptors, target).map_err(|e| anyhow!(e.to_string()))?;

    let mixer = AudioMixer::new(2, SAMPLE_RATE);
    let proxy = mixer.proxy();
    let audio = FlashAudio { mixer };

    // Give the movie a navigator rooted at its own directory so relative
    // external loads (e.g. `URLRequest("99er.mp3")` for streamed audio, or any
    // sibling asset) resolve to the files next to the SWF. Without this the
    // default null navigator has an empty base path and every relative fetch
    // fails, which can leave preloader-gated movies stuck on a black frame.
    // `NullNavigatorBackend` runs fetch futures on its own `NullExecutor`, which
    // the worker must pump each iteration (see `worker_loop`).
    let executor = NullExecutor::new();
    // `parent()` yields `Some("")` for a bare filename like `99er.swf`, which
    // can't be canonicalized; fall back to the current directory in that case.
    let base = match game.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let navigator = NullNavigatorBackend::with_base_path(base, &executor)
        .map_err(|e| anyhow!("failed to set Flash base path '{}': {e}", base.display()))?;

    let player = PlayerBuilder::new()
        .with_renderer(renderer)
        .with_audio(audio)
        .with_navigator(navigator)
        .with_movie(movie)
        .with_viewport_dimensions(width, height, 1.0)
        .with_autoplay(true)
        .build();

    // This player has no OS window, and it renders into a fixed-size offscreen
    // target sized to the movie's stage. Honoring `Stage.displayState =
    // FULL_SCREEN` would switch the stage to a fullscreen size the movie then
    // scales its content to — landing outside our fixed target and presenting as
    // a black frame. Deny fullscreen so such requests are ignored and the movie
    // keeps rendering at its native stage size; the frontend scales the output
    // to the display instead. (The default UI backend otherwise accepts the
    // request, so this must be set explicitly.)
    player.lock().unwrap().set_allow_fullscreen(false);

    Ok((player, proxy, executor, width, height, fps))
}

/// Capture the current frame as RGBA8 by downcasting the player's renderer to
/// the wgpu backend (the same technique Ruffle's exporter uses).
fn capture_frame(player: &PlayerHandle) -> Option<Vec<u8>> {
    let mut guard = player.lock().unwrap();
    let renderer = guard.renderer_mut();
    let backend = <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(renderer)?;
    backend.capture_frame().map(|img| img.into_raw())
}

#[allow(clippy::too_many_arguments)]
fn apply_cmd(
    player: &PlayerHandle,
    cmd: FlashCmd,
    width: u32,
    height: u32,
    cursor: &mut (f64, f64),
    buttons: &mut (bool, bool, bool),
    skip: &mut u32,
) {
    match cmd {
        FlashCmd::Skip(n) => *skip += n,
        FlashCmd::MouseMotion { dx, dy } => {
            cursor.0 = (cursor.0 + dx as f64).clamp(0.0, width as f64);
            cursor.1 = (cursor.1 + dy as f64).clamp(0.0, height as f64);
            player.lock().unwrap().handle_event(PlayerEvent::MouseMove {
                x: cursor.0,
                y: cursor.1,
            });
        }
        FlashCmd::MousePosition { x, y } => {
            // Absolute pointer from the frontend, mapped into stage pixels so
            // Ruffle's cursor lands where the visible OS cursor is.
            cursor.0 = (x as f64 * width as f64).clamp(0.0, width as f64);
            cursor.1 = (y as f64 * height as f64).clamp(0.0, height as f64);
            player.lock().unwrap().handle_event(PlayerEvent::MouseMove {
                x: cursor.0,
                y: cursor.1,
            });
        }
        FlashCmd::MouseButtons {
            left,
            right,
            middle,
        } => {
            let mut p = player.lock().unwrap();
            for (was, now, button) in [
                (buttons.0, left, RuffleMouseButton::Left),
                (buttons.1, right, RuffleMouseButton::Right),
                (buttons.2, middle, RuffleMouseButton::Middle),
            ] {
                if now && !was {
                    p.handle_event(PlayerEvent::MouseDown {
                        x: cursor.0,
                        y: cursor.1,
                        button,
                        index: None,
                    });
                } else if !now && was {
                    p.handle_event(PlayerEvent::MouseUp {
                        x: cursor.0,
                        y: cursor.1,
                        button,
                    });
                }
            }
            *buttons = (left, right, middle);
        }
        FlashCmd::PressKey { code, down } => {
            let mut p = player.lock().unwrap();
            if let Some(key) = retro_key_to_descriptor(code) {
                p.handle_event(if down {
                    PlayerEvent::KeyDown { key }
                } else {
                    PlayerEvent::KeyUp { key }
                });
            }
            // Text-field input: emit a character on key-down for printable keys.
            if down {
                if let Some(ch) = printable_char(code) {
                    p.handle_event(PlayerEvent::TextInput { codepoint: ch });
                }
            }
        }
        FlashCmd::Reset => {
            // A full reset would rebuild the movie; not supported for v1.
        }
        FlashCmd::SavePng(path) => {
            if let Some(rgba) = capture_frame(player) {
                if let Some(img) = image::RgbaImage::from_raw(width, height, rgba) {
                    let _ = img.save(&path);
                }
            }
        }
        FlashCmd::Unload => {}
    }
}

/// Printable ASCII for a libretro keycode (RETROK_* are ASCII-valued for the
/// printable range), used to drive Flash text fields.
fn printable_char(code: u32) -> Option<char> {
    match code {
        32..=126 => char::from_u32(code),
        _ => None,
    }
}

/// Map a libretro `retro_key` to a Ruffle [`KeyDescriptor`] for the common game
/// keys (arrows, space, enter, escape, modifiers, letters, digits). Other keys
/// are unmapped in v1.
fn retro_key_to_descriptor(code: u32) -> Option<KeyDescriptor> {
    const LETTERS: [PhysicalKey; 26] = [
        PhysicalKey::KeyA,
        PhysicalKey::KeyB,
        PhysicalKey::KeyC,
        PhysicalKey::KeyD,
        PhysicalKey::KeyE,
        PhysicalKey::KeyF,
        PhysicalKey::KeyG,
        PhysicalKey::KeyH,
        PhysicalKey::KeyI,
        PhysicalKey::KeyJ,
        PhysicalKey::KeyK,
        PhysicalKey::KeyL,
        PhysicalKey::KeyM,
        PhysicalKey::KeyN,
        PhysicalKey::KeyO,
        PhysicalKey::KeyP,
        PhysicalKey::KeyQ,
        PhysicalKey::KeyR,
        PhysicalKey::KeyS,
        PhysicalKey::KeyT,
        PhysicalKey::KeyU,
        PhysicalKey::KeyV,
        PhysicalKey::KeyW,
        PhysicalKey::KeyX,
        PhysicalKey::KeyY,
        PhysicalKey::KeyZ,
    ];
    const DIGITS: [PhysicalKey; 10] = [
        PhysicalKey::Digit0,
        PhysicalKey::Digit1,
        PhysicalKey::Digit2,
        PhysicalKey::Digit3,
        PhysicalKey::Digit4,
        PhysicalKey::Digit5,
        PhysicalKey::Digit6,
        PhysicalKey::Digit7,
        PhysicalKey::Digit8,
        PhysicalKey::Digit9,
    ];

    let desc = |physical: PhysicalKey, logical: LogicalKey, loc: KeyLocation| KeyDescriptor {
        physical_key: physical,
        logical_key: logical,
        key_location: loc,
    };

    Some(match code {
        // Arrows
        273 => desc(
            PhysicalKey::ArrowUp,
            LogicalKey::Named(NamedKey::ArrowUp),
            KeyLocation::Standard,
        ),
        274 => desc(
            PhysicalKey::ArrowDown,
            LogicalKey::Named(NamedKey::ArrowDown),
            KeyLocation::Standard,
        ),
        275 => desc(
            PhysicalKey::ArrowRight,
            LogicalKey::Named(NamedKey::ArrowRight),
            KeyLocation::Standard,
        ),
        276 => desc(
            PhysicalKey::ArrowLeft,
            LogicalKey::Named(NamedKey::ArrowLeft),
            KeyLocation::Standard,
        ),
        // Named keys
        13 => desc(
            PhysicalKey::Enter,
            LogicalKey::Named(NamedKey::Enter),
            KeyLocation::Standard,
        ),
        27 => desc(
            PhysicalKey::Escape,
            LogicalKey::Named(NamedKey::Escape),
            KeyLocation::Standard,
        ),
        8 => desc(
            PhysicalKey::Backspace,
            LogicalKey::Named(NamedKey::Backspace),
            KeyLocation::Standard,
        ),
        9 => desc(
            PhysicalKey::Tab,
            LogicalKey::Named(NamedKey::Tab),
            KeyLocation::Standard,
        ),
        32 => desc(
            PhysicalKey::Space,
            LogicalKey::Character(' '),
            KeyLocation::Standard,
        ),
        // Modifiers (left variants)
        304 => desc(
            PhysicalKey::ShiftLeft,
            LogicalKey::Named(NamedKey::Shift),
            KeyLocation::Left,
        ),
        306 => desc(
            PhysicalKey::ControlLeft,
            LogicalKey::Named(NamedKey::Control),
            KeyLocation::Left,
        ),
        // Letters a-z (RETROK_a == 97)
        97..=122 => {
            let i = (code - 97) as usize;
            desc(
                LETTERS[i],
                LogicalKey::Character(char::from(code as u8)),
                KeyLocation::Standard,
            )
        }
        // Digits 0-9 (RETROK_0 == 48)
        48..=57 => {
            let i = (code - 48) as usize;
            desc(
                DIGITS[i],
                LogicalKey::Character(char::from(code as u8)),
                KeyLocation::Standard,
            )
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    /// End-to-end smoke test: load an SWF, run a few frames, and confirm we get
    /// a full RGBA frame at the movie's dimensions. Requires a working GPU
    /// adapter, so it is `#[ignore]`d by default. Run with:
    ///   FLASH_TEST_SWF=/path/to/movie.swf cargo test flash_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn flash_smoke() {
        let path = std::env::var("FLASH_TEST_SWF")
            .expect("set FLASH_TEST_SWF to a .swf path to run this test");
        let mut emu = FlashEmu::new(Path::new(&path), HashMap::new()).expect("load SWF");

        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "movie has non-zero dimensions");
        assert!(emu.fps() > 0.0, "movie has a frame rate");

        // Pump until the worker delivers a rendered frame with real content.
        // (`run()` returns `true` unconditionally, like `RetroCoreThreaded`, so
        // detect the first frame by its pixels rather than the return value.)
        let mut got_frame = false;
        for _ in 0..600 {
            emu.run();
            emu.with_frame(&mut |_, _, buf| got_frame = buf.iter().any(|&b| b != 0));
            if got_frame {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(got_frame, "worker produced a non-empty frame");

        // Let a few more frames accumulate so animated content is visible.
        for _ in 0..30 {
            emu.run();
            std::thread::sleep(Duration::from_millis(16));
        }

        emu.with_frame(&mut |fw, fh, buf| {
            assert_eq!(fw, w);
            assert_eq!(fh, h);
            assert_eq!(buf.len(), fw * fh * 4, "RGBA8 buffer is w*h*4 bytes");
            assert!(buf.iter().any(|&b| b != 0), "frame is not all-zero");
            // Confirm real rasterized content, not just a flat clear color.
            let distinct = {
                let mut set = std::collections::HashSet::new();
                for px in buf.chunks_exact(4) {
                    set.insert([px[0], px[1], px[2]]);
                    if set.len() > 4 {
                        break;
                    }
                }
                set.len()
            };
            assert!(distinct > 1, "frame has more than one color");

            if let Ok(png) = std::env::var("FLASH_TEST_PNG") {
                let img = image::RgbaImage::from_raw(fw as u32, fh as u32, buf.to_vec())
                    .expect("valid rgba buffer");
                img.save(&png).expect("save png");
                eprintln!("wrote {png} ({fw}x{fh})");
            }
        });
    }
}
