#![allow(dead_code)]

use anyhow::{Result, anyhow};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use libloading::Library;
use tracing::{debug, error, info, trace, warn};

unsafe extern "C" {
    fn demarc_retro_log_shim(level: retro_log_level, fmt: *const c_char, ...);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn demarc_retro_log_rust(level: c_int, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let s = s.trim_end_matches(['\r', '\n']);
    match level as u32 {
        0 => debug!(target: "retro", "{s}"),
        1 => debug!(target: "retro", "{s}"),
        2 => warn!(target: "retro", "{s}"),
        _ => warn!(target: "retro", "{s}"),
    }
}

use crate::libretro::{
    RETRO_DEVICE_ID_MOUSE_LEFT, RETRO_DEVICE_ID_MOUSE_MIDDLE, RETRO_DEVICE_ID_MOUSE_RIGHT,
    RETRO_DEVICE_ID_MOUSE_X, RETRO_DEVICE_ID_MOUSE_Y, RETRO_DEVICE_MASK, RETRO_DEVICE_MOUSE,
    RETRO_ENVIRONMENT_GET_CAN_DUPE, RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION,
    RETRO_ENVIRONMENT_GET_INPUT_BITMASKS, RETRO_ENVIRONMENT_GET_LANGUAGE,
    RETRO_ENVIRONMENT_GET_LIBRETRO_PATH, RETRO_ENVIRONMENT_GET_LOG_INTERFACE,
    RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY, RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY,
    RETRO_ENVIRONMENT_GET_VARIABLE, RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE,
    RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE, RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE,
    RETRO_ENVIRONMENT_SET_GEOMETRY, RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK,
    RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO,
    RETRO_ENVIRONMENT_SET_VARIABLES, RETRO_PIXEL_FORMAT_0RGB1555, RETRO_PIXEL_FORMAT_RGB565,
    RETRO_PIXEL_FORMAT_XRGB8888, retro_audio_sample_batch_t, retro_audio_sample_t,
    retro_disk_control_callback, retro_disk_control_ext_callback, retro_environment_t,
    retro_game_geometry, retro_game_info, retro_input_poll_t, retro_input_state_t,
    retro_keyboard_callback, retro_log_callback, retro_log_level, retro_pixel_format,
    retro_system_av_info, retro_variable, retro_video_refresh_t,
};

/// Relative mouse movement accumulated since the last frame, plus button state.
/// `dx`/`dy` accumulate as i32 to avoid overflow, then clamp to i16 when the core
/// polls them, and reset to zero after each `retro_run`.
#[derive(Default)]
struct MouseState {
    dx: i32,
    dy: i32,
    left: bool,
    right: bool,
    middle: bool,
}

/// Display aspect ratio (width / height) the core wants the frame presented at.
/// Per libretro, a non-positive `aspect_ratio` means use `base_width / base_height`.
fn geometry_aspect(geom: &retro_game_geometry) -> f32 {
    if geom.aspect_ratio > 0.0 {
        geom.aspect_ratio
    } else if geom.base_height > 0 {
        geom.base_width as f32 / geom.base_height as f32
    } else {
        0.0
    }
}

trait OptionInner {
    type Inner;
}

impl<T> OptionInner for Option<T> {
    type Inner = T;
}

/// Abstract interface over a libretro emulator core. Implemented by the
/// synchronous [`RetroCore`] and by the worker-thread-backed
/// [`RetroEmuThreaded`]. Kept object-safe so callers can hold a
/// `Box<dyn RetroEmu>` and swap implementations at runtime — the frame/audio
/// accessors therefore take `&mut dyn FnMut` rather than `impl FnOnce`.
pub trait RetroEmu {
    fn set_disk(&mut self, no: u32);
    fn get_number_of_disks(&self) -> u32;
    /// Step the emulator by one presented frame (or, for the threaded variant,
    /// pull the latest frame/audio the worker has produced).
    fn run(&mut self);
    fn reset(&mut self);
    /// Cycle to the next disk image, returning the new image index.
    // fn next_disk(&mut self) -> u32;
    fn press_key(&self, code: u32, down: bool, mods: u16);
    fn add_mouse_motion(&mut self, dx: f32, dy: f32);
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool);
    /// Invoke `f` with the most recent frame as `(width, height, rgba)`.
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u8]));
    /// Invoke `f` with the audio accumulated since the last call, then clear it.
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16]));
    fn get_frame_size(&self) -> (usize, usize);
    fn aspect_ratio(&self) -> f32;
    fn sample_rate(&self) -> f64;
    fn fps(&self) -> f64;
    fn save_png(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>>;
    fn unload(&mut self);
}

#[derive(Default)]
pub struct RetroState {
    pub frame: Vec<u8>,
    pub frame_width: usize,
    pub frame_height: usize,
    pub frame_dirty: bool,
    /// Display aspect ratio reported by the core (0.0 if unknown).
    pub aspect_ratio: f32,
    /// Audio sample rate reported by the core, in Hz (0.0 if unknown).
    pub sample_rate: f64,
    pixel_format: c_int,
    fps: f64,
}

pub struct RetroCoreDirect {
    lib: Option<Library>,
    retro_run_fn: unsafe extern "C" fn(),
    retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool,
    retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info),
    retro_deinit_fn: unsafe extern "C" fn(),
    retro_reset_fn: unsafe extern "C" fn(),
    retro_set_keyboard: Option<unsafe extern "C" fn(bool, c_uint, c_uint, c_ushort)>,
    disk_callback: retro_disk_control_callback,
    state: RetroState,
    mouse: MouseState,
    vars: HashMap<String, CString>,
    audio_buf: Vec<i16>,
    core_path: CString,
    system_path: CString,
    image_index: u32,
}
impl Drop for RetroCoreDirect {
    fn drop(&mut self) {
        if self.lib.is_some() {
            unsafe { (self.retro_deinit_fn)() }
        }
    }
}

thread_local! {
    static CURRENT_EMU: Cell<*mut RetroCoreDirect> = const { Cell::new(std::ptr::null_mut()) }
}

impl RetroCoreDirect {
    pub fn unload(&mut self) {
        unsafe { (self.retro_deinit_fn)() }
        self.lib = None;
    }
    pub fn next_disk(&mut self) -> u32 {
        let cb = &self.disk_callback;
        unsafe {
            let count = (cb.get_num_images.unwrap())();
            if count < 2 {
                return 0;
            }
            self.image_index += 1;
            self.image_index %= count;
            (cb.set_eject_state.unwrap())(true);
            (cb.set_image_index.unwrap())(self.image_index);
            (cb.set_eject_state.unwrap())(false);
        }
        debug!("Inserted image {}", self.image_index);
        self.image_index
    }

    pub fn get_number_of_disks(&self) -> u32 {
        unsafe { self.disk_callback.get_num_images.map_or(0, |f| f()) }
    }

    pub fn set_disk(&self, no: u32) {
        let cb = &self.disk_callback;
        unsafe {
            cb.set_eject_state.map(|f| f(true));
            cb.set_image_index.map(|f| f(no));
            cb.set_eject_state.map(|f| f(false));
        };
    }

    pub fn with_frame(&self, f: impl FnOnce(usize, usize, &[u8])) {
        f(
            self.state.frame_width,
            self.state.frame_height,
            &self.state.frame,
        );
    }
    pub fn with_audio(&mut self, f: impl FnOnce(&[i16])) {
        f(&self.audio_buf);
        self.audio_buf.clear();
    }
    unsafe extern "C" fn input_poll_cb() {
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                if let Some(_kfn) = ctx.retro_set_keyboard {
                    // down, keycode, character, mods
                    //unsafe { kfn(true, 0, 0, 0) }
                }
            }
        });
    }
    unsafe extern "C" fn input_state_cb(
        port: c_uint,
        device: c_uint,
        index: c_uint,
        id: c_uint,
    ) -> i16 {
        let mut val: i16 = 0;
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                val = ctx.input_state(port, device, index, id);
            }
        });
        val
    }

    fn input_state(&self, port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
        if port != 0 {
            return 0;
        }
        match device & RETRO_DEVICE_MASK {
            RETRO_DEVICE_MOUSE => match id {
                RETRO_DEVICE_ID_MOUSE_X => {
                    self.mouse.dx.clamp(i16::MIN as i32, i16::MAX as i32) as i16
                }
                RETRO_DEVICE_ID_MOUSE_Y => {
                    self.mouse.dy.clamp(i16::MIN as i32, i16::MAX as i32) as i16
                }
                RETRO_DEVICE_ID_MOUSE_LEFT => self.mouse.left as i16,
                RETRO_DEVICE_ID_MOUSE_RIGHT => self.mouse.right as i16,
                RETRO_DEVICE_ID_MOUSE_MIDDLE => self.mouse.middle as i16,
                _ => 0,
            },
            _ => 0,
        }
    }
    unsafe extern "C" fn audio_sample_cb(left: i16, right: i16) {
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                ctx.audio_buf.push(left);
                ctx.audio_buf.push(right);
            }
        });
    }
    unsafe extern "C" fn audio_sample_batch_cb(data: *const i16, frames: usize) -> usize {
        if !data.is_null() && frames > 0 {
            let samples = unsafe { std::slice::from_raw_parts(data, frames * 2) };
            CURRENT_EMU.with(|p| {
                let ptr = p.get();
                if !ptr.is_null() {
                    let ctx = unsafe { &mut *ptr };
                    let take = samples.len();
                    ctx.audio_buf.extend(&samples[..take]);
                }
            });
        }
        frames
    }

    unsafe extern "C" fn video_refresh_cb(
        data: *const c_void,
        width: c_uint,
        height: c_uint,
        pitch: usize,
    ) {
        if data.is_null() {
            return;
        }
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                let slice: &[u8] = unsafe {
                    std::slice::from_raw_parts(data as *const u8, pitch * height as usize)
                };
                ctx.video_refresh(slice, width as usize, height as usize, pitch);
            }
        });
    }

    fn video_refresh(&mut self, data: &[u8], width: usize, height: usize, pitch: usize) {
        let state = &mut self.state;
        state.frame_width = width;
        state.frame_height = height;
        let needed = width * height * 4;
        if state.frame.len() != needed {
            state.frame.resize(needed, 0);
        }
        let pixel_format = state.pixel_format as retro_pixel_format;
        match pixel_format {
            RETRO_PIXEL_FORMAT_XRGB8888 => {
                for y in 0..height {
                    let src_row = &data[y * pitch..];
                    let dst_row = &mut state.frame[y * width * 4..(y + 1) * width * 4];
                    for x in 0..width {
                        let b = src_row[x * 4];
                        let g = src_row[x * 4 + 1];
                        let r = src_row[x * 4 + 2];
                        dst_row[x * 4] = r;
                        dst_row[x * 4 + 1] = g;
                        dst_row[x * 4 + 2] = b;
                        dst_row[x * 4 + 3] = 255;
                    }
                }
            }
            RETRO_PIXEL_FORMAT_RGB565 => {
                for y in 0..height {
                    let src_row = &data[y * pitch..];
                    let dst_row = &mut state.frame[y * width * 4..(y + 1) * width * 4];
                    for x in 0..width {
                        let p: u16 = src_row[x * 2] as u16 | ((src_row[x * 2 + 1] as u16) << 8);
                        let r5 = ((p >> 11) & 0x1f) as u8;
                        let g6 = ((p >> 5) & 0x3f) as u8;
                        let b5 = (p & 0x1f) as u8;
                        dst_row[x * 4] = (r5 << 3) | (r5 >> 2);
                        dst_row[x * 4 + 1] = (g6 << 2) | (g6 >> 4);
                        dst_row[x * 4 + 2] = (b5 << 3) | (b5 >> 2);
                        dst_row[x * 4 + 3] = 255;
                    }
                }
            }
            RETRO_PIXEL_FORMAT_0RGB1555 => {
                for y in 0..height {
                    let src_row = &data[y * pitch..];
                    let dst_row = &mut state.frame[y * width * 4..(y + 1) * width * 4];
                    for x in 0..width {
                        let p: u16 = src_row[x * 2] as u16 | ((src_row[x * 2 + 1] as u16) << 8);
                        let r5 = ((p >> 10) & 0x1f) as u8;
                        let g5 = ((p >> 5) & 0x1f) as u8;
                        let b5 = (p & 0x1f) as u8;
                        dst_row[x * 4] = (r5 << 3) | (r5 >> 2);
                        dst_row[x * 4 + 1] = (g5 << 3) | (g5 >> 2);
                        dst_row[x * 4 + 2] = (b5 << 3) | (b5 >> 2);
                        dst_row[x * 4 + 3] = 255;
                    }
                }
            }
            _ => {}
        }
        state.frame_dirty = true;
    }

    unsafe extern "C" fn environment_cb(cmd: c_uint, data: *mut c_void) -> bool {
        let mut ret = false;
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                ret = ctx.environment(cmd, data);
            } else {
                error!("!! FAILED ENV {cmd}");
            }
        });
        ret
    }
    fn environment(&mut self, cmd: u32, data: *mut c_void) -> bool {
        trace!("## ENV {cmd}");
        let mut handled = true;
        unsafe {
            match cmd {
                RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => {
                    let avinfo = &(*(data as *mut retro_system_av_info));
                    self.state.aspect_ratio = geometry_aspect(&avinfo.geometry);
                    self.state.sample_rate = avinfo.timing.sample_rate;
                    self.state.fps = avinfo.timing.fps;
                    info!(
                        "Got AV_INFO FPS {} RATE {} ASPECT {}",
                        avinfo.timing.fps, avinfo.timing.sample_rate, self.state.aspect_ratio
                    );
                }
                RETRO_ENVIRONMENT_SET_GEOMETRY => {
                    let geom = &(*(data as *mut retro_game_geometry));
                    self.state.aspect_ratio = geometry_aspect(geom);
                    info!("Got GEOMETRY ASPECT {}", self.state.aspect_ratio);
                }
                RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK => {
                    let callback = data as *mut retro_keyboard_callback;
                    self.retro_set_keyboard = (*callback).callback;
                }
                RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE => {
                    info!("Got DISK_CONTROL_EXT");
                    let callback = data as *mut retro_disk_control_ext_callback;
                    let retro_disk_control_ext_callback {
                        set_eject_state,
                        get_eject_state,
                        get_image_index,
                        set_image_index,
                        get_num_images,
                        replace_image_index,
                        add_image_index,
                        ..
                    } = *callback;
                    self.disk_callback = retro_disk_control_callback {
                        set_eject_state,
                        get_eject_state,
                        get_image_index,
                        set_image_index,
                        get_num_images,
                        replace_image_index,
                        add_image_index,
                    };
                }
                RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {
                    info!("Logger registered");
                    (*(data as *mut retro_log_callback)).log = Some(demarc_retro_log_shim);
                }
                RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE => {
                    info!("Got DISK_CONTROL");
                    let callback = data as *mut retro_disk_control_callback;
                    self.disk_callback = *callback;
                }
                RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY | RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
                    *(data as *mut *const c_char) = self.system_path.as_ptr();
                }
                RETRO_ENVIRONMENT_GET_LIBRETRO_PATH => {
                    *(data as *mut *const c_char) = self.core_path.as_ptr();
                }
                RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
                    let fmt = *(data as *const c_int);
                    self.state.pixel_format = fmt;
                }
                RETRO_ENVIRONMENT_GET_CAN_DUPE => {
                    *(data as *mut bool) = true;
                }
                RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                    *(data as *mut bool) = false;
                }
                RETRO_ENVIRONMENT_SET_VARIABLES => {
                    if !data.is_null() {
                        let mut p = data as *const retro_variable;
                        while !(*p).key.is_null() {
                            let key = CStr::from_ptr((*p).key).to_string_lossy().into_owned();
                            if !(*p).value.is_null() {
                                let value = CStr::from_ptr((*p).value).to_string_lossy();
                                // Format: "Description; default|opt2|opt3|..."
                                if let Some((_, opts)) = value.split_once("; ") {
                                    let default = opts.split('|').next().unwrap_or("").trim();
                                    self.set_var(&key, default);
                                }
                            }
                            p = p.add(1);
                        }
                    }
                    debug!("{:?}", self.vars);
                }
                RETRO_ENVIRONMENT_GET_VARIABLE => {
                    let var = &mut *(data as *mut retro_variable);
                    if !var.key.is_null() {
                        let key = CStr::from_ptr(var.key).to_string_lossy();
                        if let Some(value) = self.vars.get(key.as_ref()) {
                            // Safe: the CString lives in the static OPTIONS map
                            // and is never mutated after SET_VARIABLES.
                            var.value = value.as_ptr();
                        }
                    } else {
                        var.value = std::ptr::null();
                        handled = false;
                    }
                }
                RETRO_ENVIRONMENT_GET_LANGUAGE => {
                    *(data as *mut c_uint) = 0;
                }
                RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
                    *(data as *mut c_uint) = 0;
                }
                RETRO_ENVIRONMENT_GET_INPUT_BITMASKS => {}
                _ => handled = false,
            }
        }
        handled
    }

    fn set_var(&mut self, name: &str, val: impl Into<String>) {
        let v = CString::new(val.into()).unwrap();
        self.vars.insert(name.into(), v);
    }

    pub fn new(
        core_path: &Path,
        system_dir: &Path,
        game: Option<&Path>,
        settings: HashMap<String, String>,
    ) -> Result<Self> {
        let lib = unsafe { Library::new(core_path)? };
        unsafe {
            let retro_set_environment: libloading::Symbol<
                unsafe extern "C" fn(<retro_environment_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_environment")?;
            let retro_set_video_refresh: libloading::Symbol<
                unsafe extern "C" fn(<retro_video_refresh_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_video_refresh")?;
            let retro_set_audio_sample: libloading::Symbol<
                unsafe extern "C" fn(<retro_audio_sample_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_audio_sample")?;
            let retro_set_audio_sample_batch: libloading::Symbol<
                unsafe extern "C" fn(<retro_audio_sample_batch_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_audio_sample_batch")?;
            let retro_set_input_poll: libloading::Symbol<
                unsafe extern "C" fn(<retro_input_poll_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_input_poll")?;
            let retro_set_input_state: libloading::Symbol<
                unsafe extern "C" fn(<retro_input_state_t as OptionInner>::Inner),
            > = lib.get(b"retro_set_input_state")?;
            let retro_init: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_init")?;
            let retro_load_game: libloading::Symbol<
                unsafe extern "C" fn(*const retro_game_info) -> bool,
            > = lib.get(b"retro_load_game")?;
            let retro_get_system_av_info: libloading::Symbol<
                unsafe extern "C" fn(*mut retro_system_av_info),
            > = lib.get(b"retro_get_system_av_info")?;

            let retro_run_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_run")?;
            let retro_deinit_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_deinit")?;
            let retro_reset_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_reset")?;
            // let retro_unload_game_sym: libloading::Symbol<unsafe extern "C" fn()> =
            //     lib.get(b"retro_unload_game")?;

            let retro_run_fn: unsafe extern "C" fn() = *retro_run_sym;
            let retro_deinit_fn: unsafe extern "C" fn() = *retro_deinit_sym;
            let retro_reset_fn: unsafe extern "C" fn() = *retro_reset_sym;
            let retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info) =
                *retro_get_system_av_info;
            //let retro_unload_game_fn: unsafe extern "C" fn() = *retro_unload_game_sym;
            let retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool =
                *retro_load_game;

            let mut retro_emu = RetroCoreDirect {
                lib: None,
                retro_run_fn,
                retro_load_game_fn,
                retro_get_avinfo_fn,
                retro_deinit_fn,
                retro_reset_fn,
                retro_set_keyboard: None,
                disk_callback: retro_disk_control_callback::default(),
                state: Default::default(),
                mouse: Default::default(),
                vars: Default::default(),
                audio_buf: Vec::new(),
                system_path: CString::new(system_dir.to_string_lossy().as_bytes()).unwrap(),
                core_path: CString::new(core_path.to_string_lossy().as_bytes()).unwrap(),
                image_index: 0,
            };
            CURRENT_EMU.with(|p| p.set(&mut retro_emu as *mut _));
            retro_set_environment(Self::environment_cb);
            retro_set_video_refresh(Self::video_refresh_cb);
            retro_set_audio_sample(Self::audio_sample_cb);
            retro_set_audio_sample_batch(Self::audio_sample_batch_cb);
            retro_set_input_poll(Self::input_poll_cb);
            retro_set_input_state(Self::input_state_cb);

            for (key, val) in settings.iter() {
                retro_emu.set_var(key, val);
            }

            info!("retro_init()");
            retro_init();

            if let Some(game) = game {
                info!("retro_load_game({})", game.to_string_lossy());
                retro_emu.load_game(game)?;
            } else {
                if !(retro_emu.retro_load_game_fn)(std::ptr::null_mut()) {
                    return Err(anyhow!("retro_load_game failed"));
                }
            }

            let mut av_info = retro_system_av_info::default();
            retro_get_avinfo_fn(&mut av_info);
            retro_emu.state.aspect_ratio = geometry_aspect(&av_info.geometry);
            retro_emu.state.sample_rate = av_info.timing.sample_rate;
            retro_emu.state.fps = av_info.timing.fps;
            CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
            info!("avinfo: {:?}", av_info);
            retro_emu.lib = Some(lib);
            Ok(retro_emu)
        }
    }

    pub fn reset(&mut self) {
        unsafe { (self.retro_reset_fn)() }
    }

    fn load_game(&mut self, game_path: &Path) -> Result<()> {
        // puae mounts a directory as a virtual hard drive, and for any
        // need_fullpath content it loads from the path itself. In those cases
        // there are no bytes to hand over (and reading a directory errors with
        // IsADirectory), so pass data=null/size=0 and let the core use `path`.
        let game_data = if game_path.is_dir() {
            None
        } else {
            Some(std::fs::read(game_path)?)
        };
        // Pass an absolute path: cores like puae resolve m3u playlist entries
        // relative to the playlist file's own directory, so a bare relative
        // filename leaves them with no base dir and they insert zero disks.
        let abs_path = std::fs::canonicalize(game_path).unwrap_or_else(|_| game_path.to_path_buf());
        let path_str = abs_path.to_string_lossy();
        // Windows canonicalize() adds \\?\ (extended-length path prefix) which most
        // C libraries including libretro cores don't understand — strip it.
        let path_str = path_str.strip_prefix(r"\\?\").unwrap_or(path_str.as_ref());
        let game_path_c = CString::new(path_str.as_bytes())?;
        let game_info = retro_game_info {
            path: game_path_c.as_ptr(),
            data: game_data
                .as_ref()
                .map_or(std::ptr::null(), |d| d.as_ptr() as *const c_void),
            size: game_data.as_ref().map_or(0, |d| d.len()),
            meta: std::ptr::null(),
        };
        info!("Loading {:?}", game_path);
        if !unsafe { (self.retro_load_game_fn)(&game_info) } {
            return Err(anyhow!("retro_load_game({}) failed", game_path.display()));
        }
        Ok(())
    }
    pub fn run(&mut self) {
        CURRENT_EMU.with(|p| p.set(self as *mut _));
        unsafe { (self.retro_run_fn)() }
        // Relative motion has been consumed by the core this frame.
        self.mouse.dx = 0;
        self.mouse.dy = 0;
        CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
        CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
        let mut av_info = retro_system_av_info::default();
        unsafe { (self.retro_get_avinfo_fn)(&mut av_info) }
        self.state.aspect_ratio = geometry_aspect(&av_info.geometry);
        self.state.sample_rate = av_info.timing.sample_rate;
        self.state.fps = av_info.timing.fps;
    }

    /// Display aspect ratio (width / height) the core wants, or 0.0 if unknown.
    pub fn aspect_ratio(&self) -> f32 {
        self.state.aspect_ratio
    }

    /// Audio sample rate the core wants, in Hz, or 0.0 if unknown.
    pub fn sample_rate(&self) -> f64 {
        self.state.sample_rate
    }

    pub fn save_png(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let width = self.state.frame_width as u32;
        let height = self.state.frame_height as u32;
        let expected = (width as usize) * (height as usize) * 4;
        if width == 0 || height == 0 || self.state.frame.len() < expected {
            return Err("no frame available".into());
        }
        let buf = image::RgbaImage::from_raw(width, height, self.state.frame[..expected].to_vec())
            .ok_or("failed to build image buffer")?;
        buf.save(path)?;
        Ok(())
    }

    pub(crate) fn press_key(&self, code: u32, down: bool, mods: u16) {
        if let Some(cb) = self.retro_set_keyboard {
            unsafe { cb(down, code, 0, mods) }
        }
    }

    /// Accumulate relative mouse motion (in pixels) to be polled by the core
    /// on the next `run`. Deltas are summed until consumed.
    pub(crate) fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.mouse.dx = self.mouse.dx.saturating_add(dx.round() as i32);
        self.mouse.dy = self.mouse.dy.saturating_add(dy.round() as i32);
    }

    pub(crate) fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.mouse.left = left;
        self.mouse.right = right;
        self.mouse.middle = middle;
    }

    pub(crate) fn get_frame_size(&self) -> (usize, usize) {
        (self.state.frame_width, self.state.frame_height)
    }

    pub(crate) fn fps(&self) -> f64 {
        self.state.fps
    }
}

/// Thin delegation to [`RetroCore`]'s inherent methods. Fully-qualified calls
/// (`RetroCore::method(self, ..)`) are used so the inherent method is selected
/// rather than recursing into the trait method of the same name.
impl RetroEmu for RetroCoreDirect {
    fn run(&mut self) {
        RetroCoreDirect::run(self)
    }
    fn reset(&mut self) {
        RetroCoreDirect::reset(self)
    }
    fn set_disk(&mut self, no: u32) {
        RetroCoreDirect::set_disk(self, no);
    }
    fn get_number_of_disks(&self) -> u32 {
        RetroCoreDirect::get_number_of_disks(&self)
    }

    fn press_key(&self, code: u32, down: bool, mods: u16) {
        RetroCoreDirect::press_key(self, code, down, mods)
    }
    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        RetroCoreDirect::add_mouse_motion(self, dx, dy)
    }
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        RetroCoreDirect::set_mouse_buttons(self, left, right, middle)
    }
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u8])) {
        RetroCoreDirect::with_frame(self, |w, h, fr| f(w, h, fr))
    }
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        RetroCoreDirect::with_audio(self, |s| f(s))
    }
    fn get_frame_size(&self) -> (usize, usize) {
        RetroCoreDirect::get_frame_size(self)
    }
    fn aspect_ratio(&self) -> f32 {
        RetroCoreDirect::aspect_ratio(self)
    }
    fn sample_rate(&self) -> f64 {
        RetroCoreDirect::sample_rate(self)
    }
    fn fps(&self) -> f64 {
        RetroCoreDirect::fps(self)
    }
    fn save_png(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        RetroCoreDirect::save_png(self, path)
    }
    fn unload(&mut self) {
        RetroCoreDirect::unload(self)
    }
}

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
    SetDisk {
        no: u32,
    },
    SavePng {
        path: PathBuf,
    },
    Unload,
}

/// A single stepped frame's worth of data, pushed from the worker to the main
/// thread. `frame` is an RGBA copy; `audio` is the audio produced for this step
/// (accumulated on the main side, never dropped).
struct RetroUpdate {
    width: usize,
    height: usize,
    frame: Vec<u8>,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
}

/// A [`RetroEmu`] that owns its [`RetroCore`] on a dedicated worker thread. The
/// worker is free-running: it steps the core continuously at the core's
/// reported FPS and pushes each frame's data over a channel. The main thread
/// sends input/control commands over a second channel and reads a cached
/// snapshot of the latest frame/audio (refreshed in [`run`](RetroEmuThreaded::run)).
pub struct RetroCoreThreaded {
    cmd_tx: mpsc::Sender<RetroCmd>,
    update_rx: mpsc::Receiver<RetroUpdate>,
    handle: Option<thread::JoinHandle<()>>,
    frame: Vec<u8>,
    frame_width: usize,
    frame_height: usize,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
    disk_count: u32,
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
        settings: HashMap<String, String>,
    ) -> Result<Self> {
        // Own the args so they can move into the worker thread, which is where
        // the RetroCore must be both constructed and run (the CURRENT_EMU
        // thread_local set during init must match the thread retro_run uses).
        let core_path = core_path.to_path_buf();
        let system_dir = system_dir.to_path_buf();
        let game = game.map(|g| g.to_path_buf());

        let (cmd_tx, cmd_rx) = mpsc::channel::<RetroCmd>();
        let (update_tx, update_rx) = mpsc::sync_channel::<RetroUpdate>(1);
        let (setup_tx, setup_rx) = mpsc::channel::<Result<SetupResult, String>>();

        let handle = thread::Builder::new()
            .name("retro-emu".into())
            .spawn(move || {
                let mut core = match RetroCoreDirect::new(
                    &core_path,
                    &system_dir,
                    game.as_deref(),
                    settings,
                ) {
                    Ok(core) => {
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
                worker_loop(&mut core, &cmd_rx, &update_tx);
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
                update_rx,
                handle: Some(handle),
                frame: Vec::new(),
                frame_width: width,
                frame_height: height,
                audio: Vec::new(),
                aspect_ratio: 0.0,
                sample_rate: 0.0,
                fps,
                disk_count: disks,
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

/// Worker-thread main loop: apply pending commands, step the core, push the
/// resulting frame/audio. Exits when the command channel is Disconnected
/// (the `RetroEmuThreaded` was dropped) or `Unload` is received.
fn worker_loop(
    core: &mut RetroCoreDirect,
    cmd_rx: &mpsc::Receiver<RetroCmd>,
    update_tx: &mpsc::SyncSender<RetroUpdate>,
) {
    loop {
        // Drain all pending commands without blocking.
        loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    if apply_cmd(core, cmd) {
                        return; // Unload
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return,
            }
        }

        core.run();

        let (width, height) = core.get_frame_size();
        let mut frame = Vec::new();
        core.with_frame(|_, _, fr| frame.extend_from_slice(fr));
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
        };
        if update_tx.send(update).is_err() {
            return; // main side gone
        }
    }
}

/// Apply one command to the core. Returns `true` if the worker should stop.
fn apply_cmd(core: &mut RetroCoreDirect, cmd: RetroCmd) -> bool {
    match cmd {
        RetroCmd::Reset => core.reset(),
        RetroCmd::PressKey { code, down, mods } => core.press_key(code, down, mods),
        RetroCmd::AddMouseMotion { dx, dy } => core.add_mouse_motion(dx, dy),
        RetroCmd::SetMouseButtons {
            left,
            right,
            middle,
        } => core.set_mouse_buttons(left, right, middle),
        RetroCmd::SetDisk { no } => {
            core.set_disk(no);
        }
        RetroCmd::SavePng { path } => {
            let _res = core.save_png(&path).map_err(|e| e.to_string());
        }
        RetroCmd::Unload => return true,
    }
    false
}

impl RetroEmu for RetroCoreThreaded {
    fn run(&mut self) {
        if let Ok(update) = self.update_rx.recv() {
            self.frame = update.frame;
            self.frame_width = update.width;
            self.frame_height = update.height;
            self.audio.extend_from_slice(&update.audio);
            self.aspect_ratio = update.aspect_ratio;
            self.sample_rate = update.sample_rate;
            self.fps = update.fps;
        } else {
            panic!("No frame");
        }
    }
    fn get_number_of_disks(&self) -> u32 {
        self.disk_count
    }
    fn reset(&mut self) {
        let _ = self.cmd_tx.send(RetroCmd::Reset);
    }
    fn set_disk(&mut self, no: u32) {
        if self.cmd_tx.send(RetroCmd::SetDisk { no }).is_err() {}
    }
    fn press_key(&self, code: u32, down: bool, mods: u16) {
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
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u8])) {
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
        self.aspect_ratio
    }
    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
    fn fps(&self) -> f64 {
        self.fps
    }
    fn save_png(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        self.cmd_tx
            .send(RetroCmd::SavePng {
                path: path.to_path_buf(),
            })
            .map_err(|_| "retro worker thread is gone")?;
        Ok(())
    }
    fn unload(&mut self) {
        let _ = self.cmd_tx.send(RetroCmd::Unload);
    }
}

impl Drop for RetroCoreThreaded {
    fn drop(&mut self) {
        // Ask the worker to stop. It only checks for Unload at the top of its
        // loop, but with a bounded update channel it may currently be parked in
        // a full `update_tx.send()`. Keep draining the channel so that send
        // completes and the worker can loop back, observe the Unload, and
        // return — otherwise the join below would deadlock. `recv` returns Err
        // once the worker has returned and dropped its SyncSender.
        let _ = self.cmd_tx.send(RetroCmd::Unload);
        while self.update_rx.recv().is_ok() {}
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Compile-time guarantee that the core can be moved onto the worker thread.
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _check() {
        _assert_send::<RetroCoreDirect>();
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retro_amiga_works() {
        let core_path = Path::new("libretro-uae/puae_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("demos/rebels.adf");

        let settings = HashMap::new();

        let mut retro_emu =
            RetroCoreDirect::new(core_path, system_dir, Some(game_path), settings).unwrap();
        println!("## RUN");
        for _ in 0..400 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_amiga.png")).unwrap();
    }

    /// Boot a self-booting directory under Kickstart 1.3 (A500). The WHDLoad
    /// helper must be disabled, otherwise its Startup-Sequence runs `FAILAT`,
    /// a command that doesn't exist under 1.3, and the boot fails.
    #[test]
    fn retro_amiga_dir_works() {
        let core_path = Path::new("libretro-uae/puae_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("test");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A500".into());
        settings.insert("puae_use_whdload".into(), "disabled".into());

        let mut retro_emu =
            RetroCoreDirect::new(core_path, system_dir, Some(game_path), settings).unwrap();
        for _ in 0..600 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_amiga_dir.png")).unwrap();
    }

    #[test]
    fn retro_threaded_works() {
        let core_path = Path::new("libretro-uae/puae_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("demos/rebels.adf");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A500".into());

        let mut emu =
            RetroCoreThreaded::new(core_path, system_dir, Some(game_path), settings).unwrap();
        // Object-safety / interchangeability check.
        let emu: &mut dyn RetroEmu = &mut emu;

        for i in 0..400 {
            emu.run();
        }
        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "no frame produced by worker");
        emu.save_png(Path::new("test_amiga_threaded.png")).unwrap();
    }

    /// Run two threaded UAE cores and two threaded VICE cores concurrently for
    /// 400 frames each, then screenshot every instance. Each `RetroCoreThreaded`
    /// owns its own worker thread and core, so this exercises whether several
    /// threaded emulators can step in parallel without trampling each other's
    /// state. The four PNGs let us eyeball that each instance booted and
    /// produced a distinct, sensible frame.
    ///
    /// `dlopen` returns the same mapping (and the same C globals) for a core
    /// loaded twice from the same path, so two instances of one core would
    /// otherwise stomp each other and crash. We copy each core to a uniquely
    /// named file first — the trick libretro frontends use for "core duping" —
    /// so every instance gets its own mapping with independent global state.
    #[test]
    fn retro_threaded_multi_works() {
        let uae_core = Path::new("libretro-uae/puae_libretro.so");
        let vice_core = Path::new("vice-libretro/vice_x64_libretro.so");
        let system_dir = Path::new("system");
        let uae_game = Path::new("demos/rebels.adf");
        let vice_game = Path::new("demos/quantum_icc2026_v1p.prg");

        // Copy `src` to a uniquely-named .so in a kept temp dir and return its
        // path, so dlopen maps it as a separate object with its own globals.
        let dupe = |src: &Path, tag: &str| -> PathBuf {
            let dir = tempfile::Builder::new()
                .prefix("demarc-core-")
                .tempdir()
                .unwrap()
                .keep();
            let dst = dir.join(format!("{tag}.so"));
            std::fs::copy(src, &dst).unwrap();
            dst
        };

        let uae_settings = || {
            let mut s = HashMap::new();
            s.insert("puae_model".to_string(), "A500".to_string());
            s
        };

        // Distinct on-disk copy per instance.
        let cores = [
            (
                dupe(uae_core, "uae0"),
                uae_game,
                uae_settings(),
                "test_threaded_uae_0.png",
            ),
            (
                dupe(uae_core, "uae1"),
                uae_game,
                uae_settings(),
                "test_threaded_uae_1.png",
            ),
            (
                dupe(vice_core, "vice0"),
                vice_game,
                HashMap::new(),
                "test_threaded_vice_0.png",
            ),
            (
                dupe(vice_core, "vice1"),
                vice_game,
                HashMap::new(),
                "test_threaded_vice_1.png",
            ),
        ];

        // Spin up all four instances; each immediately starts a free-running
        // worker thread, so by the time construction returns they are already
        // stepping concurrently.
        let mut emus: Vec<(&str, RetroCoreThreaded)> = cores
            .iter()
            .map(|(core, game, settings, png)| {
                let emu =
                    RetroCoreThreaded::new(core, system_dir, Some(game), settings.clone()).unwrap();
                (*png, emu)
            })
            .collect();

        // Pull 400 frames from each, interleaved so they all make progress
        // together rather than one draining fully before the next starts.
        for i in 0..400 {
            println!("RUN {i}");
            for (_, emu) in emus.iter_mut() {
                let emu: &mut dyn RetroEmu = emu;
                emu.run();
            }
        }

        for (path, emu) in emus.iter_mut() {
            let (w, h) = emu.get_frame_size();
            assert!(w > 0 && h > 0, "no frame produced by worker for {path}");
            emu.save_png(Path::new(path)).unwrap();
        }
    }

    #[test]
    fn retro_vice_works() {
        let core_path = Path::new("vice-libretro/vice_x64_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("demos/quantum_icc2026_v1p.prg");

        let mut retro_emu =
            RetroCoreDirect::new(core_path, system_dir, Some(game_path), HashMap::new()).unwrap();
        println!("## RUN");
        for _ in 0..600 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_d64.png")).unwrap();
    }
}
