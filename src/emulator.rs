use std::collections::HashMap;
use std::path::Path;

use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::{image::Image, prelude::*};

use wgpu::{Extent3d, TextureDimension, TextureFormat};

use crate::audio::AudioSink;
use crate::libretro;
use crate::retro::create_core;
use crate::retro_emu::{RetroCoreThreaded, RetroEmu};
use crate::utils::{SystemType, WorkingFile, handle_file};

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

/// One libretro emulator instance, rendered into its own [`Self::image`]
/// texture. Stored as a component so several can coexist as separate entities,
/// each driven independently by `run_retro` and presented by its own
/// `PostProcess` camera (matched via [`Self::image`]).
#[derive(Component, Default)]
pub(crate) struct Emulator {
    pub(crate) core: Option<RetroCoreThreaded>,
    pub(crate) work_file: WorkingFile,
    pub(crate) run_next: bool,
    pub(crate) next_frame: f64,
    pub(crate) start_time: f64,
    pub(crate) max_time: Option<usize>,
    pub(crate) display_fps: f64,
    pub(crate) match_fps: bool,
    pub(crate) show_info: bool,
    pub(crate) match_frames: usize,
    pub(crate) tags: HashMap<String, String>,
    pub(crate) sink: AudioSink,
    pub(crate) key_map: HashMap<KeyCode, libretro::retro_key>,
    /// Integral accumulator for the audio-buffer PI controller.
    pub(crate) audio_buf_integral: f64,
    /// Latest fractional sample-rate correction from the PI controller,
    /// applied to the resampler input rate in [`Emulator::update`].
    pub(crate) audio_rate_adjust: f64,
    pub(crate) disk_no: u32,
    /// RGBA render target this emulator's frames are copied into; the matching
    /// `PostProcess` camera samples it (`PostProcess::source == image`).
    pub(crate) image: Handle<Image>,
    /// Current dimensions of [`Self::image`], tracked to detect size changes.
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) paused: bool,
    pub(crate) skipping: bool,
    /// Routing of cursor keys + Enter: keyboard (default) or a joystick port.
    pub(crate) input_mode: InputMode,
}

/// Audio ring-buffer fill level (in f32 samples) the PI controller aims to
/// hold. Sits between the duplicate (2000) and frame-drop (12000) thresholds,
/// leaving headroom on both sides.
const AUDIO_BUF_MIN: usize = 3000;
const AUDIO_BUF_TARGET: f64 = 8000.0;
const AUDIO_BUF_MAX: usize = 15000;
/// Proportional / integral gains for the audio-buffer controller. Error is
/// normalized by [`AUDIO_BUF_TARGET`], so these are dimensionless.
const AUDIO_PI_KP: f64 = 0.002 * 2.0;
const AUDIO_PI_KI: f64 = 0.0005 * 2.0;
/// Largest fractional sample-rate correction the controller may request
/// (±0.5%), enough to absorb display/audio clock drift without audible pitch.
const AUDIO_RATE_MAX_ADJUST: f64 = 0.005;

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
            TextureFormat::Rgba8UnormSrgb,
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

    pub fn update(&mut self) {
        let Emulator {
            core,
            sink,
            audio_rate_adjust,
            ..
        } = self;
        let Some(core) = core else {
            return;
        };
        // Apply the PI controller's drift correction to the resampler ratio.
        sink.set_adjust(*audio_rate_adjust);
        //resampler.set_adjust(*audio_rate_adjust);
        let from = core.sample_rate();
        core.with_audio(&mut |samples| {
            if samples.is_empty() {
                return;
            }
            sink.push_audio(from as f32, samples);
        });
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
            KeyCode::Enter => Some(RETRO_DEVICE_ID_JOYPAD_B),
            _ => None,
        }
    }

    pub fn feed_inputs(
        &mut self,
        input: &ButtonInput<KeyCode>,
        mouse_buttons: &ButtonInput<MouseButton>,
        mouse_motion: &AccumulatedMouseMotion,
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
            if *e == KeyCode::F12 {
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
            if *e == KeyCode::F12 {
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
        self.core.as_mut().unwrap().set_mouse_buttons(
            mouse_buttons.pressed(MouseButton::Left),
            mouse_buttons.pressed(MouseButton::Right),
            mouse_buttons.pressed(MouseButton::Middle),
        );
    }

    pub fn get_frame_size(&self) -> (usize, usize) {
        self.core.as_ref().unwrap().get_frame_size()
    }

    pub fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.core
            .as_mut()
            .unwrap()
            .set_mouse_buttons(left, right, middle);
    }

    pub fn get_number_of_disks(&self) -> u32 {
        self.core.as_ref().unwrap().get_number_of_disks()
    }

    pub fn set_disk(&mut self, no: u32) {
        self.core.as_mut().unwrap().set_disk(no);
    }

    pub fn reset(&mut self) {
        self.core.as_mut().unwrap().reset();
    }

    pub fn load(&mut self, time: &Time, game: &Path) {
        if let Ok(work_file) = handle_file(game, &self.tags) {
            self.core = None;
            let core = match create_core(
                work_file.system_type,
                &work_file.path,
                work_file.settings.clone(),
            ) {
                Ok(core) => core,
                Err(e) => {
                    error!("Could not load core for {:?}: {e:#}", work_file.system_type);
                    return;
                }
            };
            let t = work_file.system_type;
            if t == SystemType::Megadrive
                || t == SystemType::SuperNintendo
                || t == SystemType::Atari2600
            {
                self.input_mode = InputMode::Joystick1;
            }
            self.core = Some(core);
            self.work_file = work_file;
            self.run_next = false;
            self.next_frame = time.elapsed_secs_f64();
            self.start_time = time.elapsed_secs_f64();
            trace!("FRAME START");
        }
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
        let mut _fps = 60.0;
        if delta > 0.0 {
            _fps = 1.0 / delta;
            if self.display_fps == 0.0 {
                if _fps > 40.0 || _fps < 500.0 {
                    self.display_fps = _fps;
                }
            } else {
                self.display_fps = self.display_fps * 0.95 + _fps * 0.05;
            }
        }

        let Some(core) = self.core.as_mut() else {
            return true;
        };

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

        let occupied_len = self.sink.occupied_len();

        //let p = self.producer.lock().unwrap();

        trace!(
            "FRAME FPS {}/{} = {} : t={} AUDIO {}",
            _fps,
            self.display_fps,
            ratio,
            time.delta_secs(),
            occupied_len
        );
        if occupied_len > AUDIO_BUF_MAX {
            warn!("Dropping frame");
            self.next_frame += frame_time;
            return true;
        }

        // PI controller on audio-buffer fill. Output is a fractional
        // sample-rate correction (positive => buffer too full => speed input
        // up so the resampler emits fewer samples and the buffer drains;
        // negative => buffer draining too quickly => slow input down so the
        // resampler emits more samples and the buffer refills). Applied to the
        // resampler input rate in `Emulator::update`.
        let fill = occupied_len as f64;
        let error = (fill - AUDIO_BUF_TARGET) / AUDIO_BUF_TARGET;
        self.audio_buf_integral += error * delta;
        // Anti-windup: keep the integral term within the output clamp.
        let i_max = AUDIO_RATE_MAX_ADJUST / AUDIO_PI_KI;
        self.audio_buf_integral = self.audio_buf_integral.clamp(-i_max, i_max);
        let adjust = (AUDIO_PI_KP * error + AUDIO_PI_KI * self.audio_buf_integral)
            .clamp(-AUDIO_RATE_MAX_ADJUST, AUDIO_RATE_MAX_ADJUST);
        self.audio_rate_adjust = adjust;
        //info!("audio buf fill={fill:.0} err={error:+.3} adjust={adjust:+.5}");

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

        // For safety
        if occupied_len < AUDIO_BUF_MIN {
            result &= core.run();
            warn!("Duplicating frame");
            //self.core = Some(core);
            //return;
        }
        //drop(p);
        self.update();
        result
    }
}
