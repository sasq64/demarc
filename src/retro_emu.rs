#![allow(dead_code)]

use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::mem;
use std::path::Path;

use libloading::Library;
use tracing::{debug, info, trace};

use crate::libretro::{
    RETRO_ENVIRONMENT_GET_CAN_DUPE, RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION,
    RETRO_ENVIRONMENT_GET_INPUT_BITMASKS, RETRO_ENVIRONMENT_GET_LANGUAGE,
    RETRO_ENVIRONMENT_GET_LIBRETRO_PATH, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY,
    RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY, RETRO_ENVIRONMENT_GET_VARIABLE,
    RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE,
    RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE, RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK,
    RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, RETRO_ENVIRONMENT_SET_VARIABLES,
    RETRO_PIXEL_FORMAT_0RGB1555, RETRO_PIXEL_FORMAT_RGB565, RETRO_PIXEL_FORMAT_XRGB8888,
    retro_disk_control_callback, retro_disk_control_ext_callback, retro_game_info,
    retro_keyboard_callback, retro_pixel_format, retro_system_av_info, retro_variable,
};

type EnvironmentCb = unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool;
type VideoRefreshCb =
    unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize);
type InputPollCb = unsafe extern "C" fn();
type InputStateCb =
    unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16;
type AudioSampleCb = unsafe extern "C" fn(left: i16, right: i16);
type AudioSampleBatchCb = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;

#[derive(Default)]
pub struct RetroState {
    pub frame: Vec<u8>,
    pub frame_width: usize,
    pub frame_height: usize,
    pub frame_dirty: bool,
    pixel_format: c_int,
}

pub struct RetroEmu {
    lib: Option<Library>,
    retro_run_fn: unsafe extern "C" fn(),
    retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool,
    retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info),
    retro_set_keyboard: Option<unsafe extern "C" fn(bool, c_uint, c_uint, c_ushort)>,
    disk_ext_callback: Option<retro_disk_control_ext_callback>,
    disk_callback: Option<retro_disk_control_callback>,
    state: RetroState,
    vars: HashMap<String, CString>,
    audio_buf: Vec<i16>,
    core_path: CString,
    system_path: CString,
    image_index: usize,
}

thread_local! {
    static CURRENT_EMU: Cell<*mut RetroEmu> = const { Cell::new(std::ptr::null_mut()) }
}

impl RetroEmu {
    pub fn next_disk(&mut self) {
        println!("NEXT");
        if let Some(cb) = self.disk_ext_callback {
            unsafe {
                (cb.set_eject_state.unwrap())(true);
                self.image_index += 1;
                println!("INDEX {}", self.image_index);
                (cb.set_image_index.unwrap())(self.image_index as u32);
                (cb.set_eject_state.unwrap())(false);
            }
        } else if let Some(cb) = self.disk_callback {
            unsafe {
                (cb.set_eject_state.unwrap())(true);
                self.image_index += 1;
                println!("INDEX {}", self.image_index);
                (cb.set_image_index.unwrap())(self.image_index as u32);
                (cb.set_eject_state.unwrap())(false);
            }
        }
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
                if let Some(kfn) = ctx.retro_set_keyboard {
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
        //println!("{port} {device} {index:08x} {id}");
        0
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
                    const MAX_SAMPLES: usize = 48000 * 4 / 10;
                    let space = MAX_SAMPLES.saturating_sub(ctx.audio_buf.len());
                    let take = samples.len().min(space);
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
        debug!("{width}x{height} {pitch} {}", state.pixel_format);
        state.frame_width = width;
        state.frame_height = height;
        let needed = width * height * 4;
        if state.frame.len() != needed {
            state.frame.resize(needed, 0);
        }
        let pixel_format = state.pixel_format as retro_pixel_format;
        match pixel_format {
            RETRO_PIXEL_FORMAT_XRGB8888 => {
                todo!("")
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
                todo!("")
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

    pub fn set_var(&mut self, name: &str, val: impl Into<String>) {
        let v = CString::new(val.into()).unwrap();
        self.vars.insert(name.into(), v);
    }

    pub fn new(core_path: &Path, system_dir: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        unsafe {
            let lib = Library::new(core_path)?;

            let retro_set_environment: libloading::Symbol<unsafe extern "C" fn(EnvironmentCb)> =
                lib.get(b"retro_set_environment")?;
            let retro_set_video_refresh: libloading::Symbol<unsafe extern "C" fn(VideoRefreshCb)> =
                lib.get(b"retro_set_video_refresh")?;
            let retro_set_audio_sample: libloading::Symbol<unsafe extern "C" fn(AudioSampleCb)> =
                lib.get(b"retro_set_audio_sample")?;
            let retro_set_audio_sample_batch: libloading::Symbol<
                unsafe extern "C" fn(AudioSampleBatchCb),
            > = lib.get(b"retro_set_audio_sample_batch")?;
            let retro_set_input_poll: libloading::Symbol<unsafe extern "C" fn(InputPollCb)> =
                lib.get(b"retro_set_input_poll")?;
            let retro_set_input_state: libloading::Symbol<unsafe extern "C" fn(InputStateCb)> =
                lib.get(b"retro_set_input_state")?;
            let retro_init: libloading::Symbol<unsafe extern "C" fn()> = lib.get(b"retro_init")?;
            let retro_load_game: libloading::Symbol<
                unsafe extern "C" fn(*const retro_game_info) -> bool,
            > = lib.get(b"retro_load_game")?;
            let retro_get_system_av_info: libloading::Symbol<
                unsafe extern "C" fn(*mut retro_system_av_info),
            > = lib.get(b"retro_get_system_av_info")?;

            let retro_run_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_run")?;
            // let retro_deinit_sym: libloading::Symbol<unsafe extern "C" fn()> =
            //     lib.get(b"retro_deinit")?;
            // let retro_unload_game_sym: libloading::Symbol<unsafe extern "C" fn()> =
            //     lib.get(b"retro_unload_game")?;

            let retro_run_fn: unsafe extern "C" fn() = *retro_run_sym;
            //let retro_deinit_fn: unsafe extern "C" fn() = *retro_deinit_sym;
            let retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info) =
                *retro_get_system_av_info;
            //let retro_unload_game_fn: unsafe extern "C" fn() = *retro_unload_game_sym;
            let retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool =
                *retro_load_game;

            let mut retro_emu = RetroEmu {
                lib: None,
                retro_run_fn,
                retro_load_game_fn,
                retro_get_avinfo_fn,
                retro_set_keyboard: None,
                disk_ext_callback: None,
                disk_callback: None,
                state: Default::default(),
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

            retro_emu.set_var("vice_sid_extra", "none");
            retro_emu.set_var("vice_sid_model", "8580");
            //retro_emu.set_var("vice_autoloadwarp", "warp");
            //retro_emu.set_var("vice_autostart", "warp");
            //retro_emu.set_var("vice_cartridge", "rr38ppal.crt");
            retro_emu.set_var("vice_jiffydos", "enabled");

            println!("## INIT");
            retro_init();
            println!("## INIT DONE");

            let mut av_info = retro_system_av_info::default();
            retro_get_avinfo_fn(&mut av_info);
            CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
            println!("{:?}", av_info);
            retro_emu.lib = Some(lib);
            Ok(retro_emu)
        }
    }

    pub fn load_game(&mut self, game_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let game_data = std::fs::read(game_path)?;
        let game_path_c = CString::new(game_path.to_string_lossy().as_bytes())?;
        let game_info = retro_game_info {
            path: game_path_c.as_ptr(),
            data: game_data.as_ptr() as *const c_void,
            size: game_data.len(),
            meta: std::ptr::null(),
        };
        CURRENT_EMU.with(|p| p.set(self as *mut _));
        if !unsafe { (self.retro_load_game_fn)(&game_info) } {
            CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
            return Err(format!("retro_load_game({}) failed", game_path.display()).into());
        }
        let mut av_info = retro_system_av_info::default();
        unsafe { (self.retro_get_avinfo_fn)(&mut av_info) }
        CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
        println!("{:?}", av_info);
        //let audio_stream = init_audio_stream(av_info.timing.sample_rate);

        Ok(())
    }
    pub fn run(&mut self) {
        CURRENT_EMU.with(|p| p.set(self as *mut _));
        unsafe { (self.retro_run_fn)() }
        CURRENT_EMU.with(|p| p.set(std::ptr::null_mut()));
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

    pub(crate) fn press_key(&self, code: u32, arg: bool) {
        if let Some(cb) = self.retro_set_keyboard {
            unsafe { cb(arg, code, 0, 0) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retro_emu_works() {
        let core_path = Path::new("vice-libretro/vice_x64_libretro.so");
        let system_dir = Path::new("system");
        let game_path = Path::new("ne.d64");

        let mut retro_emu = RetroEmu::new(core_path, system_dir).unwrap();
        retro_emu.load_game(game_path).unwrap();
        println!("## RUN");
        for _ in 0..6000 {
            retro_emu.run();
        }
        retro_emu.save_png(Path::new("test_d64.png")).unwrap();
    }
}
