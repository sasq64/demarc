use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::{image::Image, prelude::*};

use wgpu::{Extent3d, TextureDimension, TextureFormat};

use crate::audio::AudioSink;
use crate::backend::{Backend, STATE_SKIPPING, ViewFocus, frame_bytes};
use crate::emu_file::{
    EmuFile, FileSource, GameInfo, Override, download_finished, download_started,
};
use crate::jobs::{Job, JobError, JobProgress};
use crate::libretro;
use crate::newsys::NewSys;
use crate::workfile::WorkFile;

/// Where the cursor keys and Enter are routed by [`Emulator::feed_inputs`].
/// In [`InputMode::Keyboard`] (the default) they map to the corresponding
/// retro keys; the joystick modes instead drive the d-pad and fire button of
/// libretro joypad port 0 (Joystick #1) or port 1 (Joystick #2).
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InputMode {
    #[default]
    Keyboard,
    Joystick1,
    Joystick2,
}

impl InputMode {
    /// Cycle Keyboard -> Joystick1 -> Joystick2 -> Keyboard.
    pub(crate) fn next(self) -> Self {
        match self {
            InputMode::Keyboard => InputMode::Joystick1,
            InputMode::Joystick1 => InputMode::Joystick2,
            InputMode::Joystick2 => InputMode::Keyboard,
        }
    }

    /// libretro joypad port this mode drives, or `None` for keyboard mode.
    fn joypad_port(self) -> Option<u32> {
        match self {
            InputMode::Keyboard => None,
            InputMode::Joystick1 => Some(0),
            InputMode::Joystick2 => Some(1),
        }
    }
}

/// A load started by [`Emulator::load_async`] whose download hasn't landed yet.
struct PendingLoad {
    /// The entry exactly as the frontend handed it over, still carrying its
    /// original [`FileSource`]. [`Emulator::update_load`] rebuilds it with the
    /// resolved path once the job finishes.
    emu_file: EmuFile,
    /// `(run_next, run_prev)` as they stood when the load was requested.
    /// [`Emulator::load_async`] clears them so the frontend doesn't re-request
    /// the same load every frame while the download runs; a load that fails
    /// puts them back, which is what lets tv mode carry on past a dead link in
    /// the direction it was already going.
    advance: (bool, bool),
    /// What `overrides.toml` had to say about this release, if anything. Held
    /// here because the parts of it that apply after the download — the file to
    /// start, the files to patch — are only used once the load actually runs.
    over: Option<Override>,
    job: Job<std::path::PathBuf>,
}

/// What [`Emulator::update_load`] found this frame.
pub(crate) enum LoadStatus {
    /// No load in flight.
    Idle,
    /// A download is still running; the previously loaded core, if any, keeps
    /// running meanwhile.
    Pending,
    /// The load finished this frame — `result` is `Ok` when the new core is
    /// live.
    ///
    /// `title` names the entry this was for. It is carried here because on
    /// failure there is nowhere else left to read it from:
    /// [`Emulator::work_file`] still describes whatever was loaded before.
    Done { title: String, result: Result<()> },
}

/// One libretro emulator instance, rendered into its own [`Self::image`]
/// texture. Stored as a component so several can coexist as separate entities,
/// each driven independently by `run_retro` and presented by its own
/// `PostProcess` camera (matched via [`Self::image`]).
#[derive(Component, Default)]
pub struct Emulator {
    pub core: Option<Box<dyn Backend + Send + Sync>>,
    pub work_file: WorkFile,
    pub run_next: bool,
    pub run_prev: bool,
    pub next_frame: f64,
    pub start_time: f64,
    pub max_time: Option<usize>,
    pub display_fps: f64,
    pub color_cycle: bool,
    pub match_fps: bool,
    pub show_info: bool,
    pub match_frames: usize,
    pub sink: AudioSink,
    pub key_map: HashMap<KeyCode, libretro::retro_key>,
    pub audio_rate_adjust: f64,
    pub audio_seen: bool,
    pub disk_no: u32,
    /// RGBA render target this emulator's frames are copied into; the matching
    /// `PostProcess` camera samples it (`PostProcess::source == image`).
    pub image: Handle<Image>,
    /// Current dimensions of [`Self::image`], tracked to detect size changes.
    pub width: u32,
    pub height: u32,
    /// [`Backend::frame_serial`] as of the last copy into [`Self::image`]. The
    /// display refreshes much faster than a core produces frames, so this is
    /// what keeps `run_retro` from re-uploading the same pixels every frame.
    pub frame_hash: u64,
    pub paused: bool,
    pub skipping: bool,
    /// Set while a warp indicator is on screen for this emulator, so
    /// [`Self::skip_finished`] knows there is something to take down again.
    warp_shown: bool,
    /// Routing of cursor keys + Enter: keyboard (default) or a joystick port.
    pub input_mode: InputMode,
    /// Benchmark mode: step the core once per update with no audio or pacing.
    pub speed_test: bool,
    pub is_image: bool,
    pub buttons: u32,
    pub last_active_time: f32,
    pub idle_time: f32,
    pub title_info: GameInfo,
    /// Download in flight for the next game, driven by [`Emulator::update_load`].
    pending_load: Option<PendingLoad>,
    pub load_delay_until: f64,
}

/// How long [`Emulator::load_delay_until`] holds off the next poll. Roughly the
/// handful of frames this used to be at 60Hz, but no longer tied to frame rate.
pub const LOAD_SETTLE_SECS: f64 = 0.1;

const AUDIO_BUF_MIN: usize = 3000;
const AUDIO_BUF_MAX: usize = 15000;

impl Emulator {
    pub fn build_keycode_map() -> HashMap<KeyCode, libretro::retro_key> {
        use KeyCode::*;
        use libretro::*;

        HashMap::from([
            (Backspace, RETROK_BACKSPACE),
            (Tab, RETROK_TAB),
            (Enter, RETROK_RETURN),
            (Pause, RETROK_PAUSE),
            (Escape, RETROK_ESCAPE),
            (Space, RETROK_SPACE),
            (Quote, RETROK_QUOTE),
            (Comma, RETROK_COMMA),
            (Minus, RETROK_MINUS),
            (Period, RETROK_PERIOD),
            (Slash, RETROK_SLASH),
            (Digit0, RETROK_0),
            (Digit1, RETROK_1),
            (Digit2, RETROK_2),
            (Digit3, RETROK_3),
            (Digit4, RETROK_4),
            (Digit5, RETROK_5),
            (Digit6, RETROK_6),
            (Digit7, RETROK_7),
            (Digit8, RETROK_8),
            (Digit9, RETROK_9),
            (Semicolon, RETROK_SEMICOLON),
            (Equal, RETROK_EQUALS),
            (BracketLeft, RETROK_LEFTBRACKET),
            (Backslash, RETROK_BACKSLASH),
            (BracketRight, RETROK_RIGHTBRACKET),
            (Backquote, RETROK_BACKQUOTE),
            (KeyA, RETROK_a),
            (KeyB, RETROK_b),
            (KeyC, RETROK_c),
            (KeyD, RETROK_d),
            (KeyE, RETROK_e),
            (KeyF, RETROK_f),
            (KeyG, RETROK_g),
            (KeyH, RETROK_h),
            (KeyI, RETROK_i),
            (KeyJ, RETROK_j),
            (KeyK, RETROK_k),
            (KeyL, RETROK_l),
            (KeyM, RETROK_m),
            (KeyN, RETROK_n),
            (KeyO, RETROK_o),
            (KeyP, RETROK_p),
            (KeyQ, RETROK_q),
            (KeyR, RETROK_r),
            (KeyS, RETROK_s),
            (KeyT, RETROK_t),
            (KeyU, RETROK_u),
            (KeyV, RETROK_v),
            (KeyW, RETROK_w),
            (KeyX, RETROK_x),
            (KeyY, RETROK_y),
            (KeyZ, RETROK_z),
            (Delete, RETROK_DELETE),
            (Numpad0, RETROK_KP0),
            (Numpad1, RETROK_KP1),
            (Numpad2, RETROK_KP2),
            (Numpad3, RETROK_KP3),
            (Numpad4, RETROK_KP4),
            (Numpad5, RETROK_KP5),
            (Numpad6, RETROK_KP6),
            (Numpad7, RETROK_KP7),
            (Numpad8, RETROK_KP8),
            (Numpad9, RETROK_KP9),
            (NumpadDecimal, RETROK_KP_PERIOD),
            (NumpadDivide, RETROK_KP_DIVIDE),
            (NumpadMultiply, RETROK_KP_MULTIPLY),
            (NumpadSubtract, RETROK_KP_MINUS),
            (NumpadAdd, RETROK_KP_PLUS),
            (NumpadEnter, RETROK_KP_ENTER),
            (NumpadEqual, RETROK_KP_EQUALS),
            (ArrowUp, RETROK_UP),
            (ArrowDown, RETROK_DOWN),
            (ArrowRight, RETROK_RIGHT),
            (ArrowLeft, RETROK_LEFT),
            (Insert, RETROK_INSERT),
            (Home, RETROK_HOME),
            (End, RETROK_END),
            (PageUp, RETROK_PAGEUP),
            (PageDown, RETROK_PAGEDOWN),
            (F1, RETROK_F1),
            (F2, RETROK_F2),
            (F3, RETROK_F3),
            (F4, RETROK_F4),
            (F5, RETROK_F5),
            (F6, RETROK_F6),
            (F7, RETROK_F7),
            (F8, RETROK_F8),
            (F9, RETROK_F9),
            (F10, RETROK_F10),
            (F11, RETROK_F11),
            (F12, RETROK_F12),
            (F13, RETROK_F13),
            (F14, RETROK_F14),
            (F15, RETROK_F15),
            (NumLock, RETROK_NUMLOCK),
            (CapsLock, RETROK_CAPSLOCK),
            (ScrollLock, RETROK_SCROLLOCK),
            (ShiftRight, RETROK_RSHIFT),
            (ShiftLeft, RETROK_LSHIFT),
            (ControlRight, RETROK_RCTRL),
            (ControlLeft, RETROK_LCTRL),
            (AltRight, RETROK_RALT),
            (AltLeft, RETROK_LALT),
            (SuperLeft, RETROK_LSUPER),
            (SuperRight, RETROK_RSUPER),
            (Help, RETROK_HELP),
            (PrintScreen, RETROK_PRINT),
            (ContextMenu, RETROK_MENU),
            (Power, RETROK_POWER),
            (Undo, RETROK_UNDO),
            (BrowserBack, RETROK_BROWSER_BACK),
            (BrowserForward, RETROK_BROWSER_FORWARD),
            (BrowserRefresh, RETROK_BROWSER_REFRESH),
            (BrowserStop, RETROK_BROWSER_STOP),
            (BrowserSearch, RETROK_BROWSER_SEARCH),
            (BrowserFavorites, RETROK_BROWSER_FAVORITES),
            (BrowserHome, RETROK_BROWSER_HOME),
            (AudioVolumeMute, RETROK_VOLUME_MUTE),
            (AudioVolumeDown, RETROK_VOLUME_DOWN),
            (AudioVolumeUp, RETROK_VOLUME_UP),
            (MediaTrackNext, RETROK_MEDIA_NEXT),
            (MediaTrackPrevious, RETROK_MEDIA_PREV),
            (MediaStop, RETROK_MEDIA_STOP),
            (MediaPlayPause, RETROK_MEDIA_PLAY_PAUSE),
            (LaunchMail, RETROK_LAUNCH_MAIL),
        ])
    }

    pub fn save_png(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(core) = self.core.as_ref() {
            core.with_frame(&mut |width, height, pixels| {
                let expected = width * height;
                // if width == 0 || height == 0 || emu.state.frame.len() < expected {
                //     return Err("no frame available".into());
                // }
                let bytes = frame_bytes(&pixels[..expected]).to_vec();
                let buf = image::RgbaImage::from_raw(width as u32, height as u32, bytes).unwrap();
                _ = buf.save(&path);
            });
        }
        Ok(())
    }

    pub fn new(
        images: &mut Assets<Image>,
        max_time: Option<usize>,
        color_cycle: bool,
        speed_test: bool,
    ) -> Self {
        let width = 720;
        let height = 574;
        let image = Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            TextureDimension::D2,
            vec![0u8; (width * height * 4) as usize],
            // NON-sRGB on purpose: libretro cores deliver raw display-space
            // (gamma-encoded) pixels, exactly like a RetroArch core framebuffer.
            // The `.slangp` shaders do their own gamma linearization, so this
            // texture must NOT be `Rgba8UnormSrgb` — that would make the hardware
            // sRGB-decode the frame before the shader, double-linearizing the
            // input (see the matching note in `blit.wgsl` about the output side).
            TextureFormat::Rgba8Unorm,
            RenderAssetUsages::all(),
        );

        let handle = images.add(image);
        Emulator {
            max_time,
            run_next: true,
            key_map: Self::build_keycode_map(),
            image: handle.clone(),
            width,
            height,
            color_cycle,
            speed_test,
            ..Default::default()
        }
    }

    /// Pass the frontend's view state on to the backend. See
    /// [`Backend::focus`](crate::backend::Backend::focus).
    pub fn focus(&mut self, focus: ViewFocus) {
        if let Some(core) = self.core.as_mut() {
            core.focus(focus);
        }
    }

    pub fn audio_active(&mut self, on: bool) {
        if on && self.sink.stream.is_none() {
            self.sink.activate();
        } else if !on && self.sink.stream.is_some() {
            self.sink.deactivate();
        }
    }

    /// Hands the core's pending audio to the sink. Returns whether any samples
    /// actually arrived, which is what [`Emulator::run`] reads to tell that a
    /// frame skip has run its course.
    pub fn update(&mut self) -> bool {
        let Emulator {
            core,
            sink,
            audio_rate_adjust,
            audio_seen,
            ..
        } = self;
        let Some(core) = core else {
            return false;
        };

        sink.set_adjust(*audio_rate_adjust);
        let from = core.sample_rate();
        let mut got_audio = false;
        core.with_audio(&mut |samples| {
            if samples.is_empty() {
                return;
            }
            *audio_seen = true;
            got_audio = true;
            sink.push_audio(from as f32, samples);
        });
        got_audio
    }

    /// Map the cursor keys and Enter to a `RETRO_DEVICE_ID_JOYPAD_*` button.
    /// Other keys return `None` so they keep going to the keyboard even in a
    /// joystick input mode.
    fn joypad_button(key: KeyCode) -> Option<u32> {
        use libretro::*;
        match key {
            KeyCode::ArrowUp => Some(RETRO_DEVICE_ID_JOYPAD_UP),
            KeyCode::ArrowDown => Some(RETRO_DEVICE_ID_JOYPAD_DOWN),
            KeyCode::ArrowLeft => Some(RETRO_DEVICE_ID_JOYPAD_LEFT),
            KeyCode::ArrowRight => Some(RETRO_DEVICE_ID_JOYPAD_RIGHT),
            KeyCode::KeyO => Some(RETRO_DEVICE_ID_JOYPAD_A),
            KeyCode::KeyX => Some(RETRO_DEVICE_ID_JOYPAD_B),
            KeyCode::KeyA => Some(RETRO_DEVICE_ID_JOYPAD_A),
            KeyCode::KeyB => Some(RETRO_DEVICE_ID_JOYPAD_B),
            KeyCode::KeyL => Some(RETRO_DEVICE_ID_JOYPAD_L),
            KeyCode::KeyR => Some(RETRO_DEVICE_ID_JOYPAD_R),
            KeyCode::Enter => Some(RETRO_DEVICE_ID_JOYPAD_START),
            KeyCode::Backspace => Some(RETRO_DEVICE_ID_JOYPAD_SELECT),
            _ => None,
        }
    }

    pub fn feed_inputs(
        &mut self,
        input: &ButtonInput<KeyCode>,
        mouse_buttons: &ButtonInput<MouseButton>,
        mouse_motion: &AccumulatedMouseMotion,
        // Absolute pointer in normalized frame coords when the cursor is over
        // this emulator's output. Used by pointer-driven cores (Flash).
        abs_pointer: Option<Vec2>,
    ) {
        let mut mods: u16 = libretro::RETROKMOD_NONE as u16;
        if input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight) {
            mods |= libretro::RETROKMOD_SHIFT as u16;
        }
        if input.pressed(KeyCode::ControlLeft) || input.pressed(KeyCode::ControlRight) {
            mods |= libretro::RETROKMOD_CTRL as u16;
        }
        if input.pressed(KeyCode::AltLeft) || input.pressed(KeyCode::AltRight) {
            mods |= libretro::RETROKMOD_ALT as u16;
        }
        if input.pressed(KeyCode::SuperLeft) || input.pressed(KeyCode::SuperRight) {
            mods |= libretro::RETROKMOD_META as u16;
        }
        if input.pressed(KeyCode::NumLock) {
            mods |= libretro::RETROKMOD_NUMLOCK as u16;
        }
        if input.pressed(KeyCode::CapsLock) {
            mods |= libretro::RETROKMOD_CAPSLOCK as u16;
        }
        if input.pressed(KeyCode::ScrollLock) {
            mods |= libretro::RETROKMOD_SCROLLOCK as u16;
        }
        let joypad_port = self.input_mode.joypad_port();
        for e in input.get_just_pressed() {
            if *e == KeyCode::F12 || *e == KeyCode::ControlRight || *e == KeyCode::AltRight {
                continue;
            }
            if let Some(port) = joypad_port
                && let Some(id) = Self::joypad_button(*e)
            {
                self.core.as_mut().unwrap().set_joypad(port, id, true);
            } else if let Some(code) = self.key_map.get(e) {
                self.core.as_mut().unwrap().press_key(*code, true, mods);
            }
        }
        for e in input.get_just_released() {
            if *e == KeyCode::F12 || *e == KeyCode::ControlRight || *e == KeyCode::AltRight {
                continue;
            }
            if let Some(port) = joypad_port
                && let Some(id) = Self::joypad_button(*e)
            {
                self.core.as_mut().unwrap().set_joypad(port, id, false);
            } else if let Some(code) = self.key_map.get(e) {
                self.core.as_mut().unwrap().press_key(*code, false, mods);
            }
        }

        let motion = mouse_motion.delta;
        if motion != Vec2::ZERO {
            self.core
                .as_mut()
                .unwrap()
                .add_mouse_motion(motion.x, motion.y);
        }
        // Sent after the relative motion so it is authoritative for cores that
        // track an absolute cursor (Flash); relative-mouse cores ignore it.
        if let Some(p) = abs_pointer {
            self.core.as_mut().unwrap().set_mouse_position(p.x, p.y);
        }
        let left = mouse_buttons.pressed(MouseButton::Left) | (self.buttons & 1 == 1);
        self.core.as_mut().unwrap().set_mouse_buttons(
            left,
            mouse_buttons.pressed(MouseButton::Right),
            mouse_buttons.pressed(MouseButton::Middle),
        );
        self.buttons = 0;
    }

    pub fn set_mouse_buttons(&mut self, buttons: u32) {
        self.buttons = buttons;
    }

    pub fn get_number_of_disks(&mut self) -> u32 {
        self.core.as_mut().unwrap().get_number_of_disks()
    }

    pub fn set_disk(&mut self, no: u32) {
        self.core.as_mut().unwrap().set_disk(no);
    }

    pub fn reset(&mut self) {
        self.core.as_mut().unwrap().reset();
    }

    pub fn get_info(&self) -> String {
        let system = self.work_file.get_meta_or("system", "???");
        let GameInfo {
            title,
            group,
            year,
            category,
            ..
        } = self.title_info;
        let year = if year == 0 {
            "".into()
        } else {
            format!(" ({year})")
        };
        let desc = if let Some(info) = self.core.as_ref().and_then(|c| c.get_info()) {
            info
        } else {
            if category.is_empty() {
                system
            } else {
                system + " " + category
            }
        };

        format!("\"{title}\"\n{group}{year}\n{desc}")
    }

    /// Begin loading `emu_file`, downloading it first if it is URL-backed.
    ///
    /// Returns immediately. The download runs on the I/O pool and the actual
    /// load happens in whichever [`update_load`](Self::update_load) call finds
    /// it finished — so the core currently running keeps running (and playing)
    /// until then, rather than the frontend stalling for the whole transfer.
    ///
    /// A load already in flight is abandoned; its result is discarded. That is
    /// what makes a fresh request during a slow download — picking another
    /// entry from the selector, say — take effect instead of being queued
    /// behind it.
    pub fn load_async(&mut self, emu_file: &EmuFile, over: Option<&Override>) {
        if let Some(previous) = &self.pending_load {
            previous.job.cancel();
            // The abandoned job never reaches `update_load`, so its share of
            // the counter has to be given back here.
            download_finished();
        }
        download_started();

        // Taken, not just read: leaving them set would have the frontend ask
        // for this same load again on the very next frame.
        let advance = (self.run_next, self.run_prev);
        self.run_next = false;
        self.run_prev = false;

        let name = if emu_file.game_info.title.is_empty() {
            "load"
        } else {
            emu_file.game_info.title
        }
        .to_string();

        // Only the *resolution* runs off-thread. Unpacking, conversion and core
        // creation stay on the main thread inside `load`, as before: they are
        // the parts that touch shared state, and they are not what a slow
        // mirror makes you wait on.
        let mut source = emu_file.path.clone();
        // The one part of an override that has to happen before the transfer:
        // which of the release's downloads is the demo.
        if let Some(name) = over.and_then(|o| o.download) {
            source.pick_download(name);
        }
        let job = Job::spawn(name, move |progress| {
            let path = source.resolve_with_progress(&|done, total| {
                progress.set_done(done);
                progress.set_total(total.unwrap_or(0));
            })?;
            Ok(path.clone())
        });

        self.pending_load = Some(PendingLoad {
            emu_file: emu_file.clone(),
            advance,
            over: over.cloned(),
            job,
        });
    }

    /// Drive a [`load_async`](Self::load_async) forward; call once per frame.
    ///
    /// When the download lands this calls [`load`](Self::load) with a
    /// [`FileSource::Path`], so the caller sees exactly the outcome the old
    /// synchronous `load` produced — just some frames later.
    pub fn update_load(&mut self, time: &Time, sys: &NewSys) -> LoadStatus {
        let Some(pending) = self.pending_load.as_mut() else {
            return LoadStatus::Idle;
        };
        // `poll` hands the result over exactly once, so it has to be kept here
        // rather than re-read after the `take` below.
        let Some(resolved) = pending.job.poll() else {
            return LoadStatus::Pending;
        };
        let PendingLoad {
            mut emu_file,
            advance,
            over,
            ..
        } = self.pending_load.take().expect("checked just above");
        // Past the `poll` above the download is over one way or another --
        // landed, failed or cancelled -- so it stops counting here, whichever
        // of the branches below the outcome takes.
        download_finished();

        let title = emu_file.game_info.title.to_string();
        let path = match resolved {
            Ok(path) => path,
            // Unwrap `JobError::Failed` rather than wrapping it: `load_error::classify`
            // downcasts along the error chain to tell a 404 from a dead mirror,
            // and an extra layer on top would still work but buys nothing.
            Err(JobError::Failed(err)) => {
                return self.failed_load(advance, title, err);
            }
            Err(JobError::Cancelled) => {
                return self.failed_load(advance, title, anyhow::anyhow!("load cancelled"));
            }
        };

        // Detection happens inside `NewSys::load_file`, from the file itself, so
        // handing `load` a resolved path loses nothing a URL would have told it.
        emu_file.path = FileSource::Path(path);
        match self.load(time, sys, &emu_file, over.as_ref()) {
            Ok(()) => LoadStatus::Done {
                title,
                result: Ok(()),
            },
            Err(err) => self.failed_load(advance, title, err),
        }
    }

    /// Report a load that didn't happen, re-arming the advance it consumed.
    ///
    /// Restoring `run_next`/`run_prev` leaves the frontend where the old
    /// synchronous path left it on failure: still asking to move on, so tv mode
    /// steps past the broken entry, while an interactive session clears them
    /// itself and stops on the error message.
    fn failed_load(
        &mut self,
        advance: (bool, bool),
        title: String,
        err: anyhow::Error,
    ) -> LoadStatus {
        (self.run_next, self.run_prev) = advance;
        LoadStatus::Done {
            title,
            result: Err(err),
        }
    }

    // The pair below is what a real progress bar would be built from. What the
    // UI draws today is only the count of downloads in flight
    // ([`crate::emu_file::downloads_in_progress`]), so outside the tests
    // nothing calls them — but the byte counting behind `load_progress` is
    // plumbed end to end already (`fetch` counts,
    // [`FileSource::resolve_with_progress`] forwards).

    /// True while a [`load_async`](Self::load_async) download is outstanding.
    #[allow(dead_code)]
    pub fn is_loading(&self) -> bool {
        self.pending_load.is_some()
    }

    /// Byte progress of the download in flight. Reports an unknown total for a
    /// multi-disk set and for a server that declares no size.
    #[allow(dead_code)]
    pub fn load_progress(&self) -> Option<&JobProgress> {
        self.pending_load.as_ref().map(|p| p.job.progress())
    }

    pub fn load(
        &mut self,
        time: &Time,
        sys: &NewSys,
        emu_file: &EmuFile,
        over: Option<&Override>,
    ) -> Result<()> {
        let mut source = emu_file.path.clone();
        let path = source.resolve()?;

        // `NewSys` and the `WorkFile` it builds own their meta, so the entry's
        // borrowed pairs are copied into `String`s here, at the one boundary
        // where the file list hands work off.
        let meta: HashMap<String, String> = emu_file
            .meta
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();

        self.title_info = emu_file.game_info;

        // Before `load_file`, which builds the new backend at the end of it: a
        // backend may own something the machine only has one of, and the next
        // one cannot take it until this one has let go. `musix`'s sc68 plugin
        // is the case that bites — libsc68 has a process-wide init that the
        // plugin claims per song, so a second SNDH loaded while the first is
        // still alive fails to init and no plugin is found for the file — but
        // libretro cores are widely non-reentrant in the same way.
        //
        // The cost is that a load which fails leaves nothing running rather
        // than the previous entry; the frontend already draws that state (it
        // skips an emulator with no core), and tv mode steps on to the next.
        self.core = None;

        let res = sys.load_file(path, &meta, over)?;
        let core = res.backend;
        if res.system.is_console() {
            self.input_mode = InputMode::Joystick1;
        }

        self.is_image = res.system.name().starts_with("Image");
        self.paused = self.is_image && (!self.color_cycle);

        self.core = Some(core);
        self.work_file = res.work_file;

        self.frame_hash = 0;
        self.run_next = false;
        self.audio_seen = false;
        self.next_frame = time.elapsed_secs_f64();
        self.start_time = time.elapsed_secs_f64();
        self.last_active_time = time.elapsed_secs();
        trace!("FRAME START");
        Ok(())
    }
    pub fn skip(&mut self, frames: u32) {
        // Latched even with no core to skip, so the indicator the caller puts up
        // for it is taken down again on the next frame rather than sitting there
        // until its timeout.
        self.warp_shown = true;
        let Some(core) = self.core.as_mut() else {
            return;
        };
        core.skip_frames(frames);
        info!("SKIPPING");
        self.skipping = true;
        self.paused = self.is_image;
    }

    /// True on the first frame after a [`Self::skip`] the warp indicator was
    /// shown for has run out, i.e. exactly when that indicator should come down.
    ///
    /// The backend reports the skip as started synchronously (see
    /// [`RetroCoreThreaded::skip_frames`](crate::retro_emu::RetroCoreThreaded)),
    /// so no separate "not started yet" grace period is needed here. A backend
    /// that never reports [`STATE_SKIPPING`] at all — every one but the threaded
    /// libretro core — finishes its skip inside `skip()` anyway, and so reads as
    /// done on the next frame, which is right.
    pub fn skip_finished(&mut self) -> bool {
        if !self.warp_shown {
            return false;
        }
        let skipping = self
            .core
            .as_ref()
            .is_some_and(|core| core.state() & STATE_SKIPPING != 0);
        if skipping {
            return false;
        }
        self.warp_shown = false;
        true
    }

    pub fn run(&mut self, time: &Time) -> bool {
        let delta = time.delta_secs_f64();
        if delta > 0.0 {
            let measured_fps = 1.0 / delta;
            if self.display_fps == 0.0 {
                if measured_fps > 40.0 && measured_fps < 500.0 {
                    self.display_fps = measured_fps;
                }
            } else {
                self.display_fps = self.display_fps * 0.95 + measured_fps * 0.05;
            }
        }

        let Some(core) = self.core.as_mut() else {
            return true;
        };
        let idle = core.is_idle();
        let t = time.elapsed_secs();
        if !idle {
            self.last_active_time = t;
        }
        self.idle_time = t - self.last_active_time;

        // Benchmark mode: pump the core once per update with no audio handling
        // and no frame pacing, so throughput is bound only by CPU/GPU speed.
        if self.speed_test {
            return core.run();
        }
        if self.paused {
            self.next_frame = time.elapsed_secs_f64();
            return true;
        }

        let ratio = (1.0 - self.display_fps / core.fps()).abs();
        if ratio < 0.01 && !self.match_fps {
            self.match_frames += 1;
            if self.match_frames >= 8 {
                self.match_fps = true;
                warn!("Switching to match fps");
            }
        }

        let fps = core.fps();
        let frame_time = if fps > 0.0 {
            1.0 / core.fps()
        } else {
            1.0 / 60.0
        };

        // A core with no audio (e.g. a still image) never fills the audio sink,
        // so none of the audio-buffer-driven pacing below applies to it.
        //
        // Keyed on samples actually arriving, not on the core advertising a
        // sample rate: a core can report 44.1kHz yet emit nothing at all for a
        // silent ROM, and treating that as "has audio" makes the buffer-dry check
        // below fire on every single frame, running the demo at double speed.
        let has_audio = self.audio_seen;
        let occupied_len = self.sink.occupied_len();

        //let p = self.producer.lock().unwrap();

        trace!("DELTA {delta} vs {}", self.display_fps,);

        if occupied_len > AUDIO_BUF_MAX {
            trace!("Dropping frame");
            self.next_frame += frame_time;
            return true;
        }

        let mut result = true;
        if self.match_fps {
            result = core.run();
        } else {
            let t = time.elapsed_secs_f64();
            while t >= self.next_frame {
                result = core.run();
                self.next_frame += frame_time;
            }
        }

        // For safety: if the audio buffer is running dry, advance an extra frame
        // to refill it. Only meaningful when the core actually produces audio;
        // otherwise the buffer is always empty and this would fire every frame.
        if has_audio && !self.skipping && occupied_len < AUDIO_BUF_MIN {
            result &= core.run();
            trace!("Duplicating frame");
        }
        let got_audio = self.update();

        // A skip delivers no audio at all while it runs — the worker discards
        // it and sends no updates — so the first samples to arrive afterwards
        // mark its end. Clearing the flag here re-arms the buffer-dry catch-up
        // above, which is the only thing that refills the ring buffer the skip
        // drained; left latched (as it was), the deficit is never repaid and
        // the audio callback underruns from here on. Meanwhile the integral has
        // wound down to its clamp against an emptiness the controller had no
        // say over, so reset it rather than let it unwind against real audio.
        if self.skipping && got_audio {
            self.skipping = false;
            self.audio_rate_adjust = 0.0;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use bevy::MinimalPlugins;
    use clap::Parser;

    use super::*;
    use crate::{Args, emu_file::UrlList};

    /// Spins up the task pools `load_async` needs, and nothing else.
    fn task_pools() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins);
        app.update();
        app
    }

    /// The system table with stock settings, which is all `update_load` needs
    /// to hand the resolved file on to `load`.
    fn systems() -> NewSys {
        NewSys::new(&Args::parse_from(["demarc"]))
    }

    /// Pumps `update_load` until it stops reporting `Pending`.
    fn drive_load(emu: &mut Emulator, sys: &NewSys) -> LoadStatus {
        let time = Time::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match emu.update_load(&time, sys) {
                LoadStatus::Pending => {
                    assert!(emu.is_loading(), "a pending load must report as loading");
                    assert!(Instant::now() < deadline, "load never finished");
                    std::thread::yield_now();
                }
                status => return status,
            }
        }
    }

    #[test]
    fn nothing_pending_is_idle() {
        let _app = task_pools();
        let mut emu = Emulator::default();
        assert!(!emu.is_loading());
        assert!(matches!(
            emu.update_load(&Time::default(), &systems()),
            LoadStatus::Idle
        ));
    }

    /// A failed download surfaces as `Done`, carrying the entry's title so the
    /// frontend can name it — `work_file` still describes the previous load.
    ///
    /// Port 1 refuses immediately, so this fails fast without leaving the host.
    #[test]
    fn a_failed_download_finishes_the_load() {
        let _app = task_pools();
        let mut emu = Emulator::default();

        emu.load_async(
            &EmuFile {
                path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
                game_info: GameInfo {
                    title: "Unreachable",
                    ..Default::default()
                },
                ..Default::default()
            },
            None,
        );
        assert!(emu.is_loading(), "the download starts in flight");

        let LoadStatus::Done { title, result } = drive_load(&mut emu, &systems()) else {
            panic!("expected the load to finish");
        };
        assert_eq!(title, "Unreachable");
        assert!(result.is_err(), "a refused connection cannot load");
        assert!(!emu.is_loading(), "the pending load is cleared either way");
        // Nothing was swapped in, so the emulator still has no core.
        assert!(emu.core.is_none());
    }

    /// The whole point of `update_load`: `load` is handed a resolved
    /// `FileSource::Path`, never a URL, so it never blocks on the network.
    #[test]
    fn a_local_path_reaches_load_unchanged() {
        let _app = task_pools();
        let dir = tempfile::tempdir().unwrap();
        // Nothing any system claims, so `load` fails once it gets there — after
        // the resolution step this test is about.
        let game = dir.path().join("demo.xyz");
        std::fs::write(&game, b"not really anything").unwrap();

        let mut emu = Emulator::default();
        emu.load_async(
            &EmuFile {
                path: FileSource::Path(game),
                ..Default::default()
            },
            None,
        );

        assert!(matches!(
            drive_load(&mut emu, &systems()),
            LoadStatus::Done { .. }
        ));
        assert!(!emu.is_loading());
    }

    /// `load_async` consumes the advance request, so the frontend asks for the
    /// load once rather than on every frame of the download; a failure hands it
    /// back, which is how tv mode steps past a dead link.
    #[test]
    fn the_advance_request_is_taken_and_returned_on_failure() {
        let _app = task_pools();
        let mut emu = Emulator {
            run_next: true,
            ..Default::default()
        };

        emu.load_async(
            &EmuFile {
                path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
                ..Default::default()
            },
            None,
        );
        assert!(
            !emu.run_next && !emu.run_prev,
            "the request is consumed while the download runs"
        );

        assert!(matches!(
            drive_load(&mut emu, &systems()),
            LoadStatus::Done { .. }
        ));
        assert!(emu.run_next, "a failed load re-arms the advance");
    }

    /// The backwards direction survives a failure too, so an explicit PrevFile
    /// onto a dead link keeps going backwards rather than reversing.
    #[test]
    fn a_failed_load_re_arms_the_direction_it_had() {
        let _app = task_pools();
        let mut emu = Emulator {
            run_prev: true,
            ..Default::default()
        };

        emu.load_async(
            &EmuFile {
                path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
                ..Default::default()
            },
            None,
        );
        assert!(matches!(
            drive_load(&mut emu, &systems()),
            LoadStatus::Done { .. }
        ));
        assert!(emu.run_prev && !emu.run_next);
    }

    /// Starting a second load replaces the first: only one download can be
    /// outstanding, so the frontend can't stack them up frame after frame.
    #[test]
    fn a_second_load_replaces_the_first() {
        let _app = task_pools();
        let mut emu = Emulator::default();
        let entry = |title: &'static str| EmuFile {
            path: FileSource::Url(UrlList::one("http://127.0.0.1:1/demo.zip")),
            game_info: GameInfo {
                title,
                ..Default::default()
            },
            ..Default::default()
        };

        emu.load_async(&entry("First"), None);
        emu.load_async(&entry("Second"), None);

        let sys = systems();
        let LoadStatus::Done { title, .. } = drive_load(&mut emu, &sys) else {
            panic!("expected the load to finish");
        };
        assert_eq!(title, "Second", "the newer load is the one that lands");
        assert!(matches!(
            emu.update_load(&Time::default(), &sys),
            LoadStatus::Idle
        ));
    }
}
