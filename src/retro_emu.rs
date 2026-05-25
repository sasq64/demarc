#![allow(dead_code)]

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::path::Path;

use libloading::Library;
use tracing::{info, trace, warn};

unsafe extern "C" {
    fn rupix_retro_log_shim(level: retro_log_level, fmt: *const c_char, ...);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rupix_retro_log_rust(level: c_int, msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    let s = s.trim_end_matches(['\r', '\n']);
    match level as u32 {
        0 => warn!(target: "retro", "{s}"),
        1 => warn!(target: "retro", "{s}"),
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

#[derive(Default)]
pub struct RetroState {
    pub frame: Vec<u8>,
    pub frame_width: usize,
    pub frame_height: usize,
    pub frame_dirty: bool,
    /// Display aspect ratio reported by the core (0.0 if unknown).
    pub aspect_ratio: f32,
    pixel_format: c_int,
}

pub struct RetroCore {
    lib: Option<Library>,
    retro_run_fn: unsafe extern "C" fn(),
    retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool,
    retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info),
    retro_deinit_fn: unsafe extern "C" fn(),
    retro_set_keyboard: Option<unsafe extern "C" fn(bool, c_uint, c_uint, c_ushort)>,
    disk_ext_callback: Option<retro_disk_control_ext_callback>,
    disk_callback: Option<retro_disk_control_callback>,
    state: RetroState,
    mouse: MouseState,
    vars: HashMap<String, CString>,
    audio_buf: Vec<i16>,
    core_path: CString,
    system_path: CString,
    image_index: usize,
}
impl Drop for RetroCore {
    fn drop(&mut self) {
        if self.lib.is_some() {
            unsafe { (self.retro_deinit_fn)() }
        }
    }
}

thread_local! {
    static CURRENT_EMU: Cell<*mut RetroCore> = const { Cell::new(std::ptr::null_mut()) }
}

impl RetroCore {
    pub fn unload(&mut self) {
        unsafe { (self.retro_deinit_fn)() }
        self.lib = None;
    }
    pub fn next_disk(&mut self) {
        self.image_index += 1;
        if let Some(cb) = self.disk_ext_callback {
            unsafe {
                (cb.set_eject_state.unwrap())(true);
                (cb.set_image_index.unwrap())(self.image_index as u32);
                (cb.set_eject_state.unwrap())(false);
            }
        } else if let Some(cb) = self.disk_callback {
            unsafe {
                (cb.set_eject_state.unwrap())(true);
                (cb.set_image_index.unwrap())(self.image_index as u32);
                (cb.set_eject_state.unwrap())(false);
            }
        }
        println!("INDEX {}", self.image_index);
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
                    //const MAX_SAMPLES: usize = 48000 * 4 / 10;
                    //let space = MAX_SAMPLES.saturating_sub(ctx.audio_buf.len());
                    let take = samples.len();
                    //eprintln!("Got {take} samples");
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
                println!("!! FAILED ENV {cmd}");
            }
        });
        ret
    }
    fn environment(&mut self, cmd: u32, data: *mut c_void) -> bool {
        trace!("## ENV {cmd}");
        unsafe {
            match cmd {
                RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO => {
                    let avinfo = &(*(data as *mut retro_system_av_info));
                    self.state.aspect_ratio = geometry_aspect(&avinfo.geometry);
                    info!(
                        "AV SWITCH FPS {} RATE {} ASPECT {}",
                        avinfo.timing.fps, avinfo.timing.sample_rate, self.state.aspect_ratio
                    );
                    true
                }
                RETRO_ENVIRONMENT_SET_GEOMETRY => {
                    let geom = &(*(data as *mut retro_game_geometry));
                    self.state.aspect_ratio = geometry_aspect(geom);
                    info!("GEOMETRY ASPECT {}", self.state.aspect_ratio);
                    true
                }
                RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK => {
                    let callback = data as *mut retro_keyboard_callback;
                    self.retro_set_keyboard = (*callback).callback;
                    true
                }
                RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE => {
                    println!("DISK EXT");
                    let callback = data as *mut retro_disk_control_ext_callback;
                    self.disk_ext_callback = Some(*callback);
                    true
                }
                RETRO_ENVIRONMENT_GET_LOG_INTERFACE => {
                    (*(data as *mut retro_log_callback)).log = Some(rupix_retro_log_shim);
                    true
                }
                RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE => {
                    println!("DISK");
                    let callback = data as *mut retro_disk_control_callback;
                    self.disk_callback = Some(*callback);
                    true
                }
                RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY | RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY => {
                    *(data as *mut *const c_char) = self.system_path.as_ptr();
                    true
                }
                RETRO_ENVIRONMENT_GET_LIBRETRO_PATH => {
                    *(data as *mut *const c_char) = self.core_path.as_ptr();
                    true
                }
                RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => {
                    let fmt = *(data as *const c_int);
                    self.state.pixel_format = fmt;
                    true
                }
                RETRO_ENVIRONMENT_GET_CAN_DUPE => {
                    *(data as *mut bool) = true;
                    true
                }
                RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE => {
                    *(data as *mut bool) = false;
                    true
                }
                RETRO_ENVIRONMENT_SET_VARIABLES => {
                    println!("### SET");
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
                    println!("{:?}", self.vars);
                    true
                }
                RETRO_ENVIRONMENT_GET_VARIABLE => {
                    let var = &mut *(data as *mut retro_variable);
                    if !var.key.is_null() {
                        let key = CStr::from_ptr(var.key).to_string_lossy();
                        println!("## GET {:?}", key);
                        if let Some(value) = self.vars.get(key.as_ref()) {
                            println!("## GET {:?} {:?}", key, value);
                            // Safe: the CString lives in the static OPTIONS map
                            // and is never mutated after SET_VARIABLES.
                            var.value = value.as_ptr();
                            return true;
                        }
                    }
                    var.value = std::ptr::null();
                    false
                }
                RETRO_ENVIRONMENT_GET_LANGUAGE => {
                    *(data as *mut c_uint) = 0;
                    true
                }
                RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION => {
                    *(data as *mut c_uint) = 0;
                    true
                }
                RETRO_ENVIRONMENT_GET_INPUT_BITMASKS => true,
                _ => false,
            }
        }
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
    ) -> Result<Self, Box<dyn std::error::Error>> {
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
            // let retro_unload_game_sym: libloading::Symbol<unsafe extern "C" fn()> =
            //     lib.get(b"retro_unload_game")?;

            let retro_run_fn: unsafe extern "C" fn() = *retro_run_sym;
            let retro_deinit_fn: unsafe extern "C" fn() = *retro_deinit_sym;
            let retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info) =
                *retro_get_system_av_info;
            //let retro_unload_game_fn: unsafe extern "C" fn() = *retro_unload_game_sym;
            let retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool =
                *retro_load_game;

            let mut retro_emu = RetroCore {
                lib: None,
                retro_run_fn,
                retro_load_game_fn,
                retro_get_avinfo_fn,
                retro_deinit_fn,
                retro_set_keyboard: None,
                disk_ext_callback: None,
                disk_callback: None,
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

            println!("## INIT");
            retro_init();
            println!("## INIT DONE");

            if let Some(game) = game {
                retro_emu.load_game(game)?;
            } else {
                if !(retro_emu.retro_load_game_fn)(std::ptr::null_mut()) {
                    return Err("retro_load_game failed".into());
                }
            }

            let mut av_info = retro_system_av_info::default();
            retro_get_avinfo_fn(&mut av_info);
            retro_emu.state.aspect_ratio = geometry_aspect(&av_info.geometry);
            CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
            println!("{:?}", av_info);
            retro_emu.lib = Some(lib);
            Ok(retro_emu)
        }
    }

    fn load_game(&mut self, game_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let game_data = std::fs::read(game_path)?;
        // Pass an absolute path: cores like puae resolve m3u playlist entries
        // relative to the playlist file's own directory, so a bare relative
        // filename leaves them with no base dir and they insert zero disks.
        let abs_path = std::fs::canonicalize(game_path).unwrap_or_else(|_| game_path.to_path_buf());
        let game_path_c = CString::new(abs_path.to_string_lossy().as_bytes())?;
        let game_info = retro_game_info {
            path: game_path_c.as_ptr(),
            data: game_data.as_ptr() as *const c_void,
            size: game_data.len(),
            meta: std::ptr::null(),
        };
        info!("Loading {:?}", game_path);
        if !unsafe { (self.retro_load_game_fn)(&game_info) } {
            return Err(format!("retro_load_game({}) failed", game_path.display()).into());
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
    }

    /// Display aspect ratio (width / height) the core wants, or 0.0 if unknown.
    pub fn aspect_ratio(&self) -> f32 {
        self.state.aspect_ratio
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retro_amiga_works() {
        let core_path = Path::new("libretro-uae/puae_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("rebels.adf");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A500PLUS".into());
        settings.insert("puae_kickstart".into(), "Kickstart2.0.rom".into());
        //settings.insert("puae_chipmem_size".into(), "4".into());

        let mut retro_emu =
            RetroCore::new(core_path, system_dir, Some(game_path), settings).unwrap();
        println!("## RUN");
        for _ in 0..400 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_amiga.png")).unwrap();
    }
    #[test]
    fn retro_vice_works() {
        let core_path = Path::new("vice-libretro/vice_x64_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("triad-plasmatica.prg");

        let mut retro_emu =
            RetroCore::new(core_path, system_dir, Some(game_path), HashMap::new()).unwrap();
        println!("## RUN");
        for _ in 0..600 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_d64.png")).unwrap();
    }
}
