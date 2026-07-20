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

use crate::retro_emu::Backend;

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

impl Backend for FlashEmu {
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
    fn get_number_of_disks(&mut self) -> u32 {
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
    let (player, proxy, mut executor, descriptors, width, height, fps) = match setup {
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

        let mut frame = Vec::new();
        if !capture_frame_fast(&player, &descriptors, &mut frame) {
            frame = vec![0u8; width as usize * height as usize * 4];
        }

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
    Arc<Descriptors>,
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
    // Keep a handle to the descriptors so the worker can read back the render
    // target's buffer itself (bypassing Ruffle's `capture_frame`, which spends
    // most of its time un-premultiplying alpha we don't need — see
    // `capture_frame_fast`).
    let renderer =
        WgpuRenderBackend::new(descriptors.clone(), target).map_err(|e| anyhow!(e.to_string()))?;

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

    Ok((player, proxy, executor, descriptors, width, height, fps))
}

/// Capture the current frame as RGBA8 by downcasting the player's renderer to
/// the wgpu backend (the same technique Ruffle's exporter uses). Returns
/// straight-alpha RGBA (via Ruffle's `capture_frame`); used for the rare
/// `SavePng` path where exact colors matter. The per-frame hot path uses
/// [`capture_frame_fast`] instead.
fn capture_frame(player: &PlayerHandle) -> Option<Vec<u8>> {
    let mut guard = player.lock().unwrap();
    let renderer = guard.renderer_mut();
    let backend = <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(renderer)?;
    backend.capture_frame().map(|img| img.into_raw())
}

/// Fast per-frame readback that maps the wgpu render target's staging buffer
/// directly and copies it into `out`, forcing alpha opaque.
///
/// Ruffle's own `capture_frame` (via `buffer_to_image`) spends the overwhelming
/// majority of its time in `unmultiply_alpha_rgba` — a per-pixel float divide
/// that converts premultiplied to straight alpha. Profiling the Flash backend
/// showed that pass alone at ~84% of CPU time for typical content (and ~25% for
/// vector-heavy movies). We present frames as opaque, so straight vs.
/// premultiplied alpha is irrelevant: we skip the divide entirely and just
/// de-pad the rows (the buffer is padded to `padded_bytes_per_row`) and set
/// alpha to 255. Returns `false` if the renderer isn't the wgpu backend or has
/// no readback buffer, leaving `out` untouched.
fn capture_frame_fast(player: &PlayerHandle, descriptors: &Descriptors, out: &mut Vec<u8>) -> bool {
    let mut guard = player.lock().unwrap();
    let renderer = guard.renderer_mut();
    let Some(backend) = <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(renderer)
    else {
        return false;
    };
    let Some(info) = &backend.target().buffer else {
        return false;
    };
    let (buffer, dims) = info.buffer.inner();

    let slice = buffer.slice(..);
    let (tx, rx) = channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    if descriptors
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .is_err()
        || !matches!(rx.recv(), Ok(Ok(())))
    {
        return false;
    }

    let map = slice.get_mapped_range();
    out.clear();
    out.reserve(dims.height * dims.unpadded_bytes_per_row);
    for row in map.chunks(dims.padded_bytes_per_row as usize) {
        out.extend_from_slice(&row[..dims.unpadded_bytes_per_row]);
    }
    // Present opaque regardless of the stage's premultiplied alpha.
    for px in out.chunks_exact_mut(4) {
        px[3] = 255;
    }
    drop(map);
    buffer.unmap();
    true
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
    use std::time::{Duration, Instant};

    /// Per-stage micro-benchmark of the worker's hot loop. Drives the Ruffle
    /// player stages directly (no channel / worker thread) so each stage's cost
    /// is measured in isolation. Requires a GPU adapter, so `#[ignore]`d. Run:
    ///   FLASH_TEST_SWF=/path/to/movie.swf cargo test --release flash_bench -- --ignored --nocapture
    #[test]
    #[ignore]
    fn flash_bench() {
        let path = std::env::var("FLASH_TEST_SWF")
            .expect("set FLASH_TEST_SWF to a .swf path to run this test");
        let frames: usize = std::env::var("FLASH_BENCH_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(300);
        // Isolate ActionScript bytecode interpretation: run only `tick` (timeline
        // + AVM), skipping render/capture/audio so a profiler attributes ~all
        // samples to the VM. Set FLASH_BENCH_TICK_ONLY=1.
        let tick_only = std::env::var("FLASH_BENCH_TICK_ONLY").is_ok();
        // Some SWFs are preloaders/menus that only start heavy content after a
        // click (e.g. 99er.swf's "PLAY" button). FLASH_BENCH_CLICK="x,y" (0..1
        // normalized stage coords) injects a click at frame FLASH_BENCH_CLICK_AT
        // (default 60) so the benchmark exercises the real content.
        let click_xy: Option<(f64, f64)> = std::env::var("FLASH_BENCH_CLICK").ok().and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
        });
        let click_at: usize = std::env::var("FLASH_BENCH_CLICK_AT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        let (player, proxy, mut executor, descriptors, width, height, fps) =
            build_player(Path::new(&path)).expect("build player");
        eprintln!("movie: {width}x{height} @ {fps} fps, benching {frames} frames");

        #[derive(Default)]
        struct Stat {
            total: Duration,
            max: Duration,
        }
        impl Stat {
            fn add(&mut self, d: Duration) {
                self.total += d;
                if d > self.max {
                    self.max = d;
                }
            }
            fn report(&self, name: &str, n: usize) {
                let avg = self.total.as_secs_f64() * 1e3 / n as f64;
                let max = self.max.as_secs_f64() * 1e3;
                eprintln!("  {name:<14} avg {avg:7.3} ms   max {max:7.3} ms");
            }
        }

        let (mut s_tick, mut s_render, mut s_exec, mut s_capture, mut s_mix) = (
            Stat::default(),
            Stat::default(),
            Stat::default(),
            Stat::default(),
            Stat::default(),
        );
        let mut audio_carry = 0.0f64;
        // Per-frame tick durations (ms) so we can show the amortization curve and
        // distinguish one-time load/preload cost from steady per-frame work.
        let mut tick_ms: Vec<f64> = Vec::with_capacity(frames);
        let loop_start = Instant::now();

        for frame_idx in 0..frames {
            if let Some((nx, ny)) = click_xy {
                if frame_idx == click_at {
                    let (px, py) = (nx * width as f64, ny * height as f64);
                    let mut p = player.lock().unwrap();
                    p.handle_event(PlayerEvent::MouseMove { x: px, y: py });
                    p.handle_event(PlayerEvent::MouseDown {
                        x: px,
                        y: py,
                        button: RuffleMouseButton::Left,
                        index: None,
                    });
                    p.handle_event(PlayerEvent::MouseUp {
                        x: px,
                        y: py,
                        button: RuffleMouseButton::Left,
                    });
                    eprintln!("injected click at ({px:.0},{py:.0}) on frame {frame_idx}");
                }
            }
            let live_fps = {
                let mut p = player.lock().unwrap();
                let frame_dur = FloatDuration::from_secs(1.0 / p.frame_rate().max(1.0));

                let t = Instant::now();
                p.tick(frame_dur);
                let dt = t.elapsed();
                tick_ms.push(dt.as_secs_f64() * 1e3);
                s_tick.add(dt);

                if !tick_only {
                    let t = Instant::now();
                    p.render();
                    s_render.add(t.elapsed());
                }

                p.frame_rate().max(1.0)
            };

            // Pump external fetches (e.g. streamed `.mp3`/asset loads) every frame,
            // even in tick-only mode, so click-triggered content can stream in.
            let t = Instant::now();
            executor.run();
            s_exec.add(t.elapsed());

            if tick_only {
                continue;
            }

            let t = Instant::now();
            let mut frame = Vec::new();
            capture_frame_fast(&player, &descriptors, &mut frame);
            s_capture.add(t.elapsed());

            audio_carry += SAMPLE_RATE as f64 / live_fps;
            let n = audio_carry.floor() as usize;
            audio_carry -= n as f64;
            let mut audio = vec![0i16; n * 2];
            let t = Instant::now();
            proxy.mix::<i16>(&mut audio);
            s_mix.add(t.elapsed());
        }

        let wall = loop_start.elapsed();

        // Optional PNG of the final frame, to confirm what was actually running
        // (e.g. that a click started the real content).
        if let Ok(png) = std::env::var("FLASH_TEST_PNG") {
            let mut frame = Vec::new();
            if capture_frame_fast(&player, &descriptors, &mut frame) {
                if let Some(img) = image::RgbaImage::from_raw(width, height, frame) {
                    let _ = img.save(&png);
                    eprintln!("wrote {png} ({width}x{height})");
                }
            }
        }

        // Tick amortization: bucketed averages across the run, plus median. A
        // front-loaded curve (early buckets >> late) means one-time load/preload;
        // a flat curve means genuine per-frame work.
        {
            let buckets = 10usize.min(frames);
            let per = frames / buckets.max(1);
            eprint!("\ntick ms by phase ({per} frames each): ");
            for b in 0..buckets {
                let seg = &tick_ms[b * per..(b + 1) * per];
                let avg = seg.iter().sum::<f64>() / seg.len() as f64;
                eprint!("{avg:.2} ");
            }
            let mut sorted = tick_ms.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let median = sorted[sorted.len() / 2];
            let p95 = sorted[sorted.len() * 95 / 100];
            eprintln!("\ntick median {median:.3} ms   p95 {p95:.3} ms   (avg is skewed by load)");
        }

        eprintln!("\nper-stage cost (avg/max over {frames} frames):");
        s_tick.report("tick", frames);
        s_render.report("render", frames);
        s_exec.report("executor.run", frames);
        s_capture.report("capture_frame", frames);
        s_mix.report("mix_audio", frames);
        let total_ms = wall.as_secs_f64() * 1e3;
        eprintln!(
            "\ntotal wall {total_ms:.1} ms  =>  {:.2} ms/frame  ({:.1} fps sustained, movie wants {fps} fps)",
            total_ms / frames as f64,
            frames as f64 / wall.as_secs_f64(),
        );
    }

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
