use std::collections::HashMap;

use anyhow::Result;
use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::{image::Image, prelude::*};

use wgpu::{Extent3d, TextureDimension, TextureFormat};

use crate::audio::AudioSink;
use crate::emu_file::EmuFile;
use crate::libretro::{self, RETROK_F1, RETROK_RETURN};
use crate::newsys::NewSys;
use crate::retro_emu::Backend;
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

pub enum EmuAction {
    LeftClick,
    RightClick,
    Key(KeyCode),
    Disk(u32),
}

pub struct EmuEvent {
    frame: u32,
    what: EmuAction,
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
    pub match_fps: bool,
    pub show_info: bool,
    pub match_frames: usize,
    pub tags: HashMap<String, String>,
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
    /// Routing of cursor keys + Enter: keyboard (default) or a joystick port.
    pub input_mode: InputMode,
    /// Benchmark mode: step the core once per update with no audio or pacing.
    pub speed_test: bool,
    pub is_image: bool,
    pub buttons: u32,
    pub last_active_time: f32,
    pub idle_time: f32,
    pub events: Vec<EmuEvent>,
    pub retro_replay: u32,
}

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

    pub fn new(
        images: &mut Assets<Image>,
        tags: HashMap<String, String>,
        max_time: Option<usize>,
        match_fps: bool,
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
            tags,
            max_time,
            run_next: true,
            key_map: Self::build_keycode_map(),
            image: handle.clone(),
            width,
            height,
            match_fps,
            speed_test,
            ..Default::default()
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
        if left {
            self.events.push(EmuEvent {
                frame: self.core.as_ref().unwrap().frames_stepped() as u32,
                what: EmuAction::LeftClick,
            });
        }
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
        self.events.push(EmuEvent {
            frame: self.core.as_ref().unwrap().frames_stepped() as u32,
            what: crate::emulator::EmuAction::Disk(no),
        });
        self.core.as_mut().unwrap().set_disk(no);
    }

    pub fn reset(&mut self) {
        self.core.as_mut().unwrap().reset();
    }

    pub fn load(&mut self, time: &Time, sys: &NewSys, emu_file: &EmuFile) -> Result<()> {
        let mut source = emu_file.path.clone();
        let path = source.resolve()?;

        let mut tags = emu_file.tags.clone();

        self.retro_replay = 0;
        // if work_file.system_type == SystemType::C64
        //     && (tags.get("fast_load") == Some(&"on".to_string()) && is_disk_image(&work_file.path)
        //         || has_extension(&work_file.path, "m3u"))
        // {
        //     tags.insert("vice_cartridge".into(), "rr38ppal-auto.crt".into());
        //     tags.insert("vice_autostart".into(), "disabled".into());
        //     self.retro_replay = 1;
        // }
        for (key, val) in &self.tags {
            tags.insert(key.clone(), val.clone());
        }
        let res = sys.load_file(&path, &tags)?;

        // let work_file = prepare_file(emu_file, &self.tags)?;
        // let tags = resolve_tags(&work_file);

        self.core = None;
        //let tags = load_tags(&work_file, &self.tags);
        //
        // let core = create_core(
        //     work_file.system_type,
        //     &work_file.path,
        //     tags,
        //     self.speed_test,
        // )?;
        let core = res.backend;
        // let t = work_file.system_type;
        // if t == SystemType::Megadrive
        //     || t == SystemType::SuperNintendo
        //     || t == SystemType::Atari2600
        //     || t == SystemType::Gameboy
        //     || t == SystemType::Gba
        //     || t == SystemType::Psx
        // {
        //     self.input_mode = InputMode::Joystick1;
        // }
        // if t == SystemType::Ilbm {
        //     self.is_image = true;
        //     let cycle_enabled = self.tags.get("color_cycle").is_some_and(|v| v == "enabled");
        //     self.paused = !cycle_enabled;
        // } else {
        //     // Loading a real emulator must clear any lingering still-image state
        //     // from a previously opened ILBM, otherwise the fresh core inherits
        //     // the image's paused flag and never runs.
        //     self.is_image = false;
        //     self.paused = false;
        // }

        self.core = Some(core);
        self.work_file = res.work_file;
        // The new backend's serial has nothing to do with the old one's, so
        // start from "nothing uploaded yet". A backend that begins at 0 does so
        // with a blank frame, which is exactly what the texture already holds.
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
        let Some(core) = self.core.as_mut() else {
            return;
        };
        core.skip_frames(frames);
        info!("SKIPPING");
        self.skipping = true;
        self.paused = false;
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

        if self.retro_replay > 0 {
            let frames = core.frames_stepped();
            let f = 60;
            if self.retro_replay == 1 && frames >= f {
                core.press_key(RETROK_F1, true, 0);
                self.retro_replay += 1;
            } else if self.retro_replay == 2 && frames >= f + 2 {
                core.press_key(RETROK_F1, false, 0);
                self.retro_replay += 1;
            } else if self.retro_replay == 3 && frames == f + 6 {
                core.press_key(RETROK_RETURN, true, 0);
                self.retro_replay += 1;
            } else if self.retro_replay == 4 && frames == f + 8 {
                core.press_key(RETROK_RETURN, false, 0);
                self.retro_replay = 0;
            }
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
            warn!("Dropping frame");
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
            warn!("Duplicating frame");
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
