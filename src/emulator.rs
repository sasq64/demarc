use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use bevy::asset::RenderAssetUsages;
use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::{image::Image, prelude::*};

use wgpu::{Extent3d, TextureDimension, TextureFormat};

use crate::audio::AudioSink;
use retro_core::audio_recorder::RecorderOptions;
use crate::backend::{Backend, STATE_SKIPPING, VideoFrame, ViewFocus, frame_bytes};
use crate::config::LatencySetting;
use crate::emu_file::{EmuFile, GameInfo};
use crate::libretro;
use crate::ui::{HudLocation, SetHudText};
use crate::workfile::WorkFile;

mod load;
pub(crate) use load::{LoadStatus, PendingLoad};

/// The middle square of the frame, at full height, as a PNG.
fn cover_art(shot: &image::RgbaImage) -> Result<Vec<u8>> {
    let size = shot.width().min(shot.height());
    let cropped = image::imageops::crop_imm(shot, (shot.width() - size) / 2, 0, size, size).to_image();
    let rgb = image::DynamicImage::ImageRgba8(cropped).into_rgb8();
    let mut png = Vec::new();
    rgb.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    Ok(png)
}

/// A recording stops once the core has sent no audio for this long.
const AUDIO_STOPPED_SECS: f64 = 2.0;

/// Furthest behind its frame clock an emulator catches up instead of resyncing.
const MAX_CATCH_UP: f64 = 0.25;

type AudioTap = Box<dyn Fn(u32, &[f32]) + Send>;

/// Sees every emulator's audio, for the remote control script.
static AUDIO_TAP: Mutex<Option<AudioTap>> = Mutex::new(None);

pub(crate) fn set_audio_tap(tap: impl Fn(u32, &[f32]) + Send + 'static) {
    *AUDIO_TAP.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(tap));
}

fn tap_audio(rate: u32, samples: &[f32]) {
    if let Some(tap) = AUDIO_TAP.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        tap(rate, samples);
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Recorded {
    Key(KeyCode),
    Click(Vec2),
}

#[allow(dead_code)] // TODO: Use for better cross fading
#[derive(Default)]
pub enum EmuState {
    #[default]
    Stopped,
    Loading,

    PreDelay,
    WaitNonIdle,
    CrossFadeDelay,
    CrossFade,

    InfoDelay,
    Running,
}

/// One libretro emulator instance, rendered into its own [`Self::image`]
/// texture. Stored as a component so several can coexist as separate entities,
/// each driven independently by `run_retro` and presented by its own
/// `PostProcess` camera (matched via [`Self::image`]).
#[derive(Component, Default)]
pub struct Emulator {
    pub core: Option<Box<dyn Backend + Send + Sync>>,
    pub work_file: WorkFile,
    // Request to run next or previous demo in this emulator
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
    /// The core pushes its audio into [`Self::sink`] from its own thread.
    pub self_audio: bool,
    pub key_map: HashMap<KeyCode, libretro::retro_key>,
    /// Multiplier on the resample ratio; `1.0` is no correction.
    pub audio_rate_adjust: f64,
    /// Smoothed fill of the audio sink, in samples; `0` until first measured.
    pub audio_fill: f64,
    pub audio_seen: bool,
    /// Seconds of running without any samples from the core.
    pub audio_gap: f64,
    pub disk_no: u32,
    /// RGBA render target this emulator's frames are copied into; the matching
    /// `PostProcess` camera samples it (`PostProcess::source == image`).
    pub image: Handle<Image>,
    /// Current dimensions of [`Self::image`], tracked to detect size changes.
    pub width: u32,
    pub height: u32,
    pub paused: bool,
    pub skipping: bool,
    /// Set while a warp indicator is on screen for this emulator, so
    /// [`Self::skip_finished`] knows there is something to take down again.
    pub(crate) warp_shown: bool,
    /// Routing of cursor keys + Enter: keyboard (default) or a joystick port.
    pub input_mode: InputMode,
    /// Benchmark mode: step the core once per update with no audio or pacing.
    pub speed_test: bool,
    pub is_image: bool,
    pub buttons: u32,
    /// The left button as last fed, to log each click once.
    left_down: bool,
    /// Patch menu "Record Events": the frame recording started at, and what was
    /// pressed since.
    pub recording: Option<(u64, Vec<(u64, Recorded)>)>,
    pub last_active_time: f32,
    pub idle_time: f32,
    pub emu_file: EmuFile,
    pub favorite: bool,
    /// This is the spare emulator loads are diverted into, not a view the user
    /// sees. Travels with the role, not the entity: the swap at the end of a
    /// cross-faded load moves it to the emulator that just left the screen.
    pub is_crossfade: bool,
    /// Download in flight for the next file, driven by
    /// [`Emulator::update_load`].
    pub(crate) pending_load: Option<PendingLoad>,
    pub state: EmuState,
    /// Frames waiting to be shown, oldest first.
    pub frame_queue: VecDeque<VideoFrame>,
    /// Pixels last copied into [`Self::image`], so an unchanged frame is not
    /// copied and re-uploaded again.
    pub shown_frame: Option<Arc<Vec<u32>>>,
    pub latency: LatencySetting,
    /// This emulator's `emu` column in the `--frame-log` CSV.
    log_id: usize,
}

impl Emulator {
    /// A key as `overrides.toml` names it: a `KeyCode` name, or a lone letter or digit.
    pub fn resolve_key_name(key: &str) -> Option<libretro::retro_key> {
        let name = match key {
            c if c.len() == 1 && c.as_bytes()[0].is_ascii_digit() => format!("Digit{c}"),
            c if c.len() == 1 && c.as_bytes()[0].is_ascii_alphabetic() => {
                format!("Key{}", c.to_ascii_uppercase())
            }
            name => name.to_string(),
        };
        Self::build_keycode_map()
            .into_iter()
            .find(|(k, _)| format!("{k:?}") == name)
            .map(|(_, code)| code)
    }

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

    pub fn save_png(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let mut shot = None;
        if let Some(core) = self.core.as_ref() {
            core.with_frame(&mut |width, height, pixels| {
                let expected = width * height;
                let bytes = frame_bytes(&pixels[..expected]).to_vec();
                shot = image::RgbaImage::from_raw(width as u32, height as u32, bytes);
            });
        }
        if let Some(shot) = shot {
            _ = shot.save(&path);
            if self.sink.recording_time().is_some() {
                match cover_art(&shot) {
                    Ok(png) => self.sink.set_cover(png),
                    Err(e) => warn!("cover art: {e}"),
                }
            }
        }
        Ok(())
    }

    #[hotpath::measure(impl_type = "Emulator")]
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
            audio_rate_adjust: 1.0,
            key_map: Self::build_keycode_map(),
            image: handle.clone(),
            width,
            height,
            color_cycle,
            speed_test,
            log_id: crate::frame_log::next_id(),
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

    pub fn start_recording(&mut self, time: &Time, mut opts: RecorderOptions) {
        let meta = |key| {
            let value = self.work_file.get_meta_or(key, "");
            (!value.is_empty()).then_some(value)
        };
        opts.tags = retro_core::audio_recorder::Tags {
            title: meta("title"),
            artist: meta("author"),
            album: meta("party").and_then(|p| p.split(';').next().map(str::to_owned)),
            year: meta("year").and_then(|y| y.parse().ok()),
            cover: None,
        };
        let title = self.work_file.get_meta_or("title", "audio");
        let name = format!("{}-{}.wav", title, time.elapsed_secs() as i32);
        self.sink.start_recording(name, opts);
    }

    /// Set this emulator's output level, `0` (silent) to `1`. The cross fade
    /// ramps it; everything else runs at `1`.
    pub fn set_volume(&self, level: f32) {
        self.sink.set_volume(level);
    }

    #[cfg(test)]
    pub fn volume(&self) -> f32 {
        self.sink.volume()
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
    #[hotpath::measure(impl_type = "Emulator")]
    pub fn update(&mut self) -> bool {
        let Emulator {
            core,
            sink,
            audio_rate_adjust,
            audio_seen,
            self_audio,
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
            tap_audio(from as u32, samples);
            if !*self_audio {
                sink.push_audio(from as f32, samples);
            }
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

    #[hotpath::measure(impl_type = "Emulator")]
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
        let frame = self.core.as_ref().unwrap().frames_stepped();
        let hot_key = input.any_pressed([KeyCode::AltRight, KeyCode::ControlRight]);
        for e in input.get_just_pressed() {
            if *e == KeyCode::F12 || *e == KeyCode::ControlRight || *e == KeyCode::AltRight {
                continue;
            }
            if let Some((start, events)) = &mut self.recording
                && !hot_key
                && self.key_map.contains_key(e)
            {
                events.push((frame.saturating_sub(*start), Recorded::Key(*e)));
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
        if left
            && !self.left_down
            && let Some(p) = abs_pointer
        {
            info!("{{ frame = {frame}, click = [{:.3}, {:.3}] }}", p.x, p.y);
            if let Some((start, events)) = &mut self.recording {
                events.push((frame.saturating_sub(*start), Recorded::Click(p)));
            }
        }
        self.left_down = left;
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
        let year = self.emu_file.game_info.year();
        let GameInfo {
            title,
            group,
            category,
            ..
        } = self.emu_file.game_info;
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

    /// The info text and its favorite marker, for the HUD.
    pub fn info_hud(&self, delay: std::time::Duration, duration: std::time::Duration) -> [SetHudText; 2] {
        let heart = if self.favorite { "\u{f02d1}" } else { "" };
        [
            (self.get_info(), HudLocation::InfoText),
            (heart.to_string(), HudLocation::Favorite),
        ]
        .map(|(text, location)| SetHudText {
            text,
            delay,
            duration,
            location,
        })
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

    /// Restart the idle timer, as if the core had just produced something new.
    ///
    /// Wanted whenever an idle timeout has actually been acted on: the core
    /// goes on being idle while the next release loads, so the timer has to be
    /// re-armed by hand or the timeout fires again on the very next frame.
    /// `idle_time` is set here too — [`Self::run`] only recomputes it when
    /// there is a core, and there is none in the middle of a load.
    pub fn reset_idle(&mut self, time: &Time) {
        self.last_active_time = time.elapsed_secs();
        self.idle_time = 0.0;
    }

    pub fn set_latency(&mut self, latency: LatencySetting) {
        self.sink.set_low_water(latency.audio_buf_min);
        if self.latency != latency {
            self.latency = latency;
            if let Some(core) = self.core.as_mut() {
                core.set_latency(latency.latency);
            }
        }
    }

    #[hotpath::measure(impl_type = "Emulator")]
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
        core.keep_alive();
        let idle = core.is_idle();
        let t = time.elapsed_secs();
        // A paused core stands still because it was asked to, so the timer is
        // held armed instead: an idle timeout switching the demo out from under
        // a pause is never what was wanted. Still images are paused by default
        // and do rely on the timeout to advance, hence `!self.is_image`.
        if !idle || (self.paused && !self.is_image) {
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
            // Same reasoning as the idle timer above: paused time does not
            // count towards `max_time` either.
            if !self.is_image {
                self.start_time += delta;
            }
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
        self.audio_fill = if self.audio_fill == 0.0 {
            occupied_len as f64
        } else {
            self.audio_fill * 0.95 + occupied_len as f64 * 0.05
        };
        self.audio_rate_adjust = rate_adjust(self.audio_fill, &self.latency);

        crate::frame_log::log(&crate::frame_log::FrameStats {
            emu: self.log_id,
            time: time.elapsed_secs_f64(),
            delta,
            fill: occupied_len,
            queued: core.queued_frames(),
            buf_min: self.latency.audio_buf_min,
            buf_max: self.latency.audio_buf_max,
            fps,
            cpu: core.cpu_load(),
        });

        trace!("DELTA {delta} vs {}", self.display_fps,);

        if occupied_len > self.latency.audio_buf_max {
            trace!("Dropping frame");
            self.next_frame += frame_time;
            return true;
        }

        let mut result = true;
        if self.match_fps {
            result = core.run();
        } else {
            let t = time.elapsed_secs_f64();
            // Back from a stall: catching up would flood the audio ring.
            if t - self.next_frame > MAX_CATCH_UP {
                self.next_frame = t;
            }
            while t >= self.next_frame {
                result = core.run();
                self.next_frame += frame_time;
            }
        }

        // For safety: if the audio buffer is running dry, advance an extra frame
        // to refill it. Only meaningful when the core actually produces audio;
        // otherwise the buffer is always empty and this would fire every frame.
        if has_audio && !self.skipping && occupied_len < self.latency.audio_buf_min {
            result &= core.run();
            trace!("Duplicating frame");
        }
        let got_audio = self.update();
        self.audio_gap = if got_audio || self.skipping {
            0.0
        } else {
            self.audio_gap + delta
        };
        if self.audio_gap > AUDIO_STOPPED_SECS
            && self.sink.recording_time().is_some_and(|t| !t.is_zero())
        {
            info!("recording: no audio from the core, stopping");
            self.sink.stop_on_silence();
        }

        // A skip delivers no audio at all while it runs — the worker discards
        // it and sends no updates — so the first samples to arrive afterwards
        // mark its end. Clearing the flag here re-arms the buffer-dry catch-up
        // above, which is the only thing that refills the ring buffer the skip
        // drained; left latched (as it was), the deficit is never repaid and
        // the audio callback underruns from here on. The smoothed fill tracked
        // the drained ring, so start it over from the refilled one.
        if self.skipping && got_audio {
            self.skipping = false;
            self.audio_fill = 0.0;
        }
        result
    }
}

/// Resample ratio multiplier that steers the smoothed sink `fill` down towards
/// just above `audio_buf_min`, so the ring runs near empty instead of filling.
fn rate_adjust(fill: f64, latency: &LatencySetting) -> f64 {
    let min = latency.audio_buf_min as f64;
    let window = (latency.audio_buf_max - latency.audio_buf_min) as f64;
    let target = min + window / 8.0;
    (1.0 - 0.01 * (fill - target) / window).clamp(0.99, 1.01)
}

#[cfg(test)]
#[path = "tests/emulator_tests.rs"]
mod tests;
