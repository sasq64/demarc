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
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use ruffle_core::backend::audio::{
    AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
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
/// Channel depth for frame updates; small for low latency, like the libretro
/// worker. Blocking sends provide backpressure if the frontend falls behind.
const UPDATE_QUEUE: usize = 2;

/// Commands sent from [`FlashEmu`] (main thread) to the Ruffle worker.
enum FlashCmd {
    PressKey { code: u32, down: bool },
    MouseMotion { dx: f32, dy: f32 },
    MouseButtons { left: bool, right: bool, middle: bool },
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
    /// Interleaved stereo i16 for this frame's elapsed time.
    audio: Vec<i16>,
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
        // Drain to the newest frame; accumulate all audio so nothing is lost.
        let mut got = false;
        let rx = self.update_rx.get_mut().unwrap();
        loop {
            match rx.try_recv() {
                Ok(update) => {
                    self.width = update.width;
                    self.height = update.height;
                    self.frame = update.frame;
                    self.audio.extend_from_slice(&update.audio);
                    got = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        got
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
    let (player, proxy, width, height, fps) = match setup {
        Ok(v) => v,
        Err(e) => {
            let _ = setup_tx.send(SetupResult::Err(e.to_string()));
            return;
        }
    };
    let _ = setup_tx.send(SetupResult::Ok { width, height, fps });

    let frame_dur = Duration::from_secs_f64(1.0 / fps.max(1.0));
    // Absolute cursor position in stage pixels; Ruffle wants absolute coords but
    // the RetroEmu trait only offers relative motion.
    let mut cursor = (width as f64 / 2.0, height as f64 / 2.0);
    let mut buttons = (false, false, false);
    let mut last_capture: Option<Vec<u8>> = None;
    let mut last = Instant::now();

    let profile = std::env::var("FLASH_PROFILE").is_ok();
    let mut prof_n = 0u32;
    let mut prof_tick = Duration::ZERO;
    let mut prof_render = Duration::ZERO;
    let mut prof_capture = Duration::ZERO;

    loop {
        // Apply all pending commands.
        let mut skip = 0u32;
        loop {
            match cmd_rx.try_recv() {
                Ok(FlashCmd::Unload) => return,
                Ok(cmd) => apply_cmd(&player, cmd, width, height, &mut cursor, &mut buttons, &mut skip),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        let now = Instant::now();
        let dt = now.duration_since(last);
        last = now;

        let t_tick;
        let t_render;
        {
            let mut p = player.lock().unwrap();
            // Do NOT preload with an unlimited budget here: `tick`/`run_frame`
            // already stream-preload each frame with a per-frame time box (see
            // `Player::run_frame`), so a large movie starts presenting frames
            // immediately. Calling `preload(ExecutionLimit::none())` ourselves
            // would block the worker on the *entire* movie before the first
            // frame — a multi-second freeze on big SWFs.
            for _ in 0..skip {
                p.run_frame();
            }
            let a = Instant::now();
            p.tick(FloatDuration::from_secs(dt.as_secs_f64()));
            t_tick = a.elapsed();
            let b = Instant::now();
            p.render();
            t_render = b.elapsed();
        }

        let c = Instant::now();
        let frame = capture_frame(&player)
            .or_else(|| last_capture.clone())
            .unwrap_or_else(|| vec![0u8; width as usize * height as usize * 4]);
        let t_capture = c.elapsed();
        last_capture = Some(frame.clone());
        if profile {
            prof_n += 1;
            prof_tick += t_tick;
            prof_render += t_render;
            prof_capture += t_capture;
            if prof_n == 60 {
                let live_fps = player.lock().unwrap().frame_rate();
                eprintln!(
                    "[flash] over {prof_n} frames: tick={:.2}ms render={:.2}ms capture={:.2}ms  ({:.1} fps ceiling)  live_fps={live_fps}",
                    prof_tick.as_secs_f64() * 1000.0 / 60.0,
                    prof_render.as_secs_f64() * 1000.0 / 60.0,
                    prof_capture.as_secs_f64() * 1000.0 / 60.0,
                    60.0 / (prof_tick + prof_render + prof_capture).as_secs_f64(),
                );
                prof_n = 0;
                prof_tick = Duration::ZERO;
                prof_render = Duration::ZERO;
                prof_capture = Duration::ZERO;
            }
        }

        // Pull mixed audio for the elapsed wall-clock time so the rate stays
        // correct regardless of loop jitter; the frontend resampler + PI
        // controller absorb the remaining drift.
        let n_frames = (SAMPLE_RATE as f64 * dt.as_secs_f64()).round() as usize;
        let mut audio = vec![0i16; n_frames * 2];
        proxy.mix::<i16>(&mut audio);

        // Blocking send paces the worker to the frontend's consumption.
        if update_tx
            .send(FlashUpdate {
                width: width as usize,
                height: height as usize,
                frame,
                audio,
            })
            .is_err()
        {
            return;
        }

        if let Some(remaining) = frame_dur.checked_sub(now.elapsed()) {
            thread::sleep(remaining);
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

    let player = PlayerBuilder::new()
        .with_renderer(renderer)
        .with_audio(audio)
        .with_movie(movie)
        .with_viewport_dimensions(width, height, 1.0)
        .with_autoplay(true)
        .build();

    Ok((player, proxy, width, height, fps))
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
                if let Some(img) =
                    image::RgbaImage::from_raw(width, height, rgba)
                {
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
        PhysicalKey::KeyA, PhysicalKey::KeyB, PhysicalKey::KeyC, PhysicalKey::KeyD,
        PhysicalKey::KeyE, PhysicalKey::KeyF, PhysicalKey::KeyG, PhysicalKey::KeyH,
        PhysicalKey::KeyI, PhysicalKey::KeyJ, PhysicalKey::KeyK, PhysicalKey::KeyL,
        PhysicalKey::KeyM, PhysicalKey::KeyN, PhysicalKey::KeyO, PhysicalKey::KeyP,
        PhysicalKey::KeyQ, PhysicalKey::KeyR, PhysicalKey::KeyS, PhysicalKey::KeyT,
        PhysicalKey::KeyU, PhysicalKey::KeyV, PhysicalKey::KeyW, PhysicalKey::KeyX,
        PhysicalKey::KeyY, PhysicalKey::KeyZ,
    ];
    const DIGITS: [PhysicalKey; 10] = [
        PhysicalKey::Digit0, PhysicalKey::Digit1, PhysicalKey::Digit2, PhysicalKey::Digit3,
        PhysicalKey::Digit4, PhysicalKey::Digit5, PhysicalKey::Digit6, PhysicalKey::Digit7,
        PhysicalKey::Digit8, PhysicalKey::Digit9,
    ];

    let desc = |physical: PhysicalKey, logical: LogicalKey, loc: KeyLocation| KeyDescriptor {
        physical_key: physical,
        logical_key: logical,
        key_location: loc,
    };

    Some(match code {
        // Arrows
        273 => desc(PhysicalKey::ArrowUp, LogicalKey::Named(NamedKey::ArrowUp), KeyLocation::Standard),
        274 => desc(PhysicalKey::ArrowDown, LogicalKey::Named(NamedKey::ArrowDown), KeyLocation::Standard),
        275 => desc(PhysicalKey::ArrowRight, LogicalKey::Named(NamedKey::ArrowRight), KeyLocation::Standard),
        276 => desc(PhysicalKey::ArrowLeft, LogicalKey::Named(NamedKey::ArrowLeft), KeyLocation::Standard),
        // Named keys
        13 => desc(PhysicalKey::Enter, LogicalKey::Named(NamedKey::Enter), KeyLocation::Standard),
        27 => desc(PhysicalKey::Escape, LogicalKey::Named(NamedKey::Escape), KeyLocation::Standard),
        8 => desc(PhysicalKey::Backspace, LogicalKey::Named(NamedKey::Backspace), KeyLocation::Standard),
        9 => desc(PhysicalKey::Tab, LogicalKey::Named(NamedKey::Tab), KeyLocation::Standard),
        32 => desc(PhysicalKey::Space, LogicalKey::Character(' '), KeyLocation::Standard),
        // Modifiers (left variants)
        304 => desc(PhysicalKey::ShiftLeft, LogicalKey::Named(NamedKey::Shift), KeyLocation::Left),
        306 => desc(PhysicalKey::ControlLeft, LogicalKey::Named(NamedKey::Control), KeyLocation::Left),
        // Letters a-z (RETROK_a == 97)
        97..=122 => {
            let i = (code - 97) as usize;
            desc(LETTERS[i], LogicalKey::Character(char::from(code as u8)), KeyLocation::Standard)
        }
        // Digits 0-9 (RETROK_0 == 48)
        48..=57 => {
            let i = (code - 48) as usize;
            desc(DIGITS[i], LogicalKey::Character(char::from(code as u8)), KeyLocation::Standard)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

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

        // Pump until the worker delivers at least one rendered frame.
        let mut got_frame = false;
        for _ in 0..600 {
            if emu.run() {
                got_frame = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(got_frame, "worker produced a frame");

        // Let a few more frames accumulate so animated content is visible.
        let iters = if std::env::var("FLASH_PROFILE").is_ok() {
            600
        } else {
            30
        };
        eprintln!("[flash] movie fps = {}", emu.fps());
        let mut delivered = 0u32;
        let t0 = Instant::now();
        for _ in 0..iters {
            if emu.run() {
                delivered += 1;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        if std::env::var("FLASH_PROFILE").is_ok() {
            eprintln!(
                "[flash] delivered {delivered} new frames in {:.2}s = {:.1} fps to frontend",
                t0.elapsed().as_secs_f64(),
                delivered as f64 / t0.elapsed().as_secs_f64(),
            );
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
