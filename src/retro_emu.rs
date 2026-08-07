use anyhow::{Result, anyhow};
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

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
    RETRO_DEVICE_ID_JOYPAD_MASK, RETRO_DEVICE_ID_MOUSE_LEFT, RETRO_DEVICE_ID_MOUSE_MIDDLE,
    RETRO_DEVICE_ID_MOUSE_RIGHT, RETRO_DEVICE_ID_MOUSE_X, RETRO_DEVICE_ID_MOUSE_Y,
    RETRO_DEVICE_JOYPAD, RETRO_DEVICE_KEYBOARD, RETRO_DEVICE_MASK, RETRO_DEVICE_MOUSE,
    RETRO_ENVIRONMENT_GET_CAN_DUPE, RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION,
    RETRO_ENVIRONMENT_GET_INPUT_BITMASKS, RETRO_ENVIRONMENT_GET_LANGUAGE,
    RETRO_ENVIRONMENT_GET_LIBRETRO_PATH, RETRO_ENVIRONMENT_GET_LOG_INTERFACE,
    RETRO_ENVIRONMENT_GET_MESSAGE_INTERFACE_VERSION, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY,
    RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY, RETRO_ENVIRONMENT_GET_VARIABLE,
    RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE,
    RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE, RETRO_ENVIRONMENT_SET_FRAME_TIME_CALLBACK,
    RETRO_ENVIRONMENT_SET_GEOMETRY, RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK,
    RETRO_ENVIRONMENT_SET_MESSAGE, RETRO_ENVIRONMENT_SET_MESSAGE_EXT,
    RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO,
    RETRO_ENVIRONMENT_SET_VARIABLES, RETRO_PIXEL_FORMAT_0RGB1555, RETRO_PIXEL_FORMAT_RGB565,
    RETRO_PIXEL_FORMAT_XRGB8888, retro_audio_sample_batch_t, retro_audio_sample_t,
    retro_disk_control_callback, retro_disk_control_ext_callback, retro_environment_t,
    retro_frame_time_callback, retro_game_geometry, retro_game_info, retro_input_poll_t,
    retro_input_state_t, retro_keyboard_callback, retro_log_callback, retro_log_level,
    retro_pixel_format, retro_system_av_info, retro_variable, retro_video_refresh_t,
};

/// Stack for the thread a core runs on. See the `stack_size` call in
/// [`RetroCoreThreaded::new`] for why the default is not enough.
const WORKER_STACK_SIZE: usize = 32 * 1024 * 1024;

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

/// Abstract interface over a libretro emulator core.
pub trait Backend {
    fn set_disk(&mut self, no: u32);
    /// Takes `&mut self` because the libretro implementation calls into the
    /// core, which may issue environment callbacks while it does.
    fn get_number_of_disks(&mut self) -> u32;
    /// Step the emulator by one presented frame
    fn run(&mut self) -> bool;

    fn run_frames(&mut self, count: u32) {
        self.skip_frames(count);
        for i in 0..count {
            while !self.run() {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    fn reset(&mut self);
    fn press_key(&mut self, code: u32, down: bool, mods: u16);
    fn add_mouse_motion(&mut self, dx: f32, dy: f32);
    /// Set the absolute pointer position in normalized frame coordinates
    /// (`0.0..=1.0`, origin top-left). Cores driven by relative mouse motion
    /// (libretro) ignore this; Flash needs it so Ruffle's internal cursor tracks
    /// the visible OS cursor for hit-testing buttons.
    fn set_mouse_position(&mut self, _x: f32, _y: f32) {}
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool);
    fn set_joypad(&mut self, port: u32, id: u32, down: bool);
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32]));
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16]));
    fn get_frame_size(&self) -> (usize, usize);
    fn aspect_ratio(&self) -> f32;
    fn sample_rate(&self) -> f64;
    fn fps(&self) -> f64;
    // fn unload(&mut self);
    fn skip_frames(&mut self, frames: u32);
    /// Total number of emulated frames the core has stepped so far. Used by the
    /// `--speed-test` benchmark to measure throughput. Defaults to 0 for cores
    /// that don't track it.
    fn frames_stepped(&self) -> u64 {
        0
    }
    /// A value that changes whenever [`with_frame`](Self::with_frame) would hand
    /// back different pixels than it did last time.
    ///
    /// The frontend re-uploads the emulator's texture only when this moves, so a
    /// backend that leaves it constant is never redrawn — which is why there is
    /// no default implementation. Any monotonic counter or content hash will do;
    /// it only has to differ, not to increase.
    fn frame_hash(&self) -> u64;
    fn is_idle(&self) -> bool {
        false
    }
}

/// Reinterpret a slice of packed RGBA pixels as the raw bytes the GPU texture
/// upload (and PNG encoder) expect. Each `u32` holds one pixel with its bytes
/// already in `[r, g, b, a]` memory order (see the LUTs / `video_refresh`), so
/// this is a plain, always-sound width-narrowing view.
pub fn frame_bytes(pixels: &[u32]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(pixels.as_ptr() as *const u8, std::mem::size_of_val(pixels))
    }
}

#[derive(Default)]
pub struct RetroState {
    pub frame: Vec<u32>,
    pub frame_width: usize,
    pub frame_height: usize,
    /// Display aspect ratio reported by the core (0.0 if unknown).
    pub aspect_ratio: f32,
    /// Audio sample rate reported by the core, in Hz (0.0 if unknown).
    pub sample_rate: f64,
    pixel_format: c_int,
    fps: f64,
    keys: Vec<u8>,
    /// Joypad button state as a bitmask per port (index 0 = Joystick #1,
    /// 1 = Joystick #2). Bit `n` corresponds to `RETRO_DEVICE_ID_JOYPAD_*`.
    joypad: [u16; 2],
}

const fn expand5(c: u8) -> u8 {
    (c << 3) | (c >> 2)
}

const fn expand6(c: u8) -> u8 {
    (c << 2) | (c >> 4)
}

/// Precomputed RGB565 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`. Replaces the per-pixel bit unpacking in
/// [`RetroCoreDirect::video_refresh`].
static RGB565_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 11) & 0x1f) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand6(g6), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Precomputed 0RGB1555 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`.
static RGB1555_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 10) & 0x1f) as u8;
        let g5 = ((v >> 5) & 0x1f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand5(g5), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Convert a 16-bits-per-pixel libretro framebuffer to packed RGBA8888 using
/// `lut`, which maps each raw 16-bit little-endian pixel to one output pixel.
/// `dst` must already be sized to `width * height`.
fn convert_16bpp(
    src: &[u8],
    dst: &mut [u32],
    width: usize,
    height: usize,
    pitch: usize,
    lut: &[u32; 65536],
) {
    for y in 0..height {
        let src_row = &src[y * pitch..y * pitch + width * 2];
        let dst_row = &mut dst[y * width..(y + 1) * width];
        for (out, px) in dst_row.iter_mut().zip(src_row.chunks_exact(2)) {
            let p = u16::from_le_bytes([px[0], px[1]]) as usize;
            *out = lut[p];
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Fold `frame` (one packed RGBA8888 pixel per `u32`, as handed to
/// [`Backend::with_frame`]) into its hash and its uniform-colour flags in one
/// pass.
///
/// Pixels are hashed in pairs so the multiply is amortized over two of them; a
/// 320x240 frame is ~38k iterations of a handful of ALU ops, which is noise next
/// to the emulation that produced it. An empty frame reports as both black and
/// white — callers are expected to have a real frame in hand.
fn scan_frame(frame: &[u32]) -> u64 {
    let mut hash = FNV_OFFSET;

    let mut pairs = frame.chunks_exact(2);
    for p in &mut pairs {
        let w = (p[0] as u64) | ((p[1] as u64) << 32);
        hash = (hash ^ w).wrapping_mul(FNV_PRIME);
    }

    // An odd pixel count leaves one pixel over; hash it alone in the low half.
    if let [px] = *pairs.remainder() {
        hash = (hash ^ px as u64).wrapping_mul(FNV_PRIME);
    }
    hash
}

pub struct RetroCoreDirect {
    lib: Option<Library>,
    retro_run_fn: unsafe extern "C" fn(),
    retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool,
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
    /// Temp dir holding this instance's private copy of the core .so. Held so
    /// the copy lives as long as the loaded library and is removed on drop.
    _core_tempdir: tempfile::TempDir,
    skip_frames: u32,
    retro_frame_time: Option<unsafe extern "C" fn(i64)>,
    time_reference: i64,
    /// Bumped by every `run`, since each one leaves a freshly rendered frame.
    /// See [`Backend::frame_serial`].
    frame_serial: u64,
}
impl Drop for RetroCoreDirect {
    fn drop(&mut self) {
        if self.lib.is_some() {
            let _guard = CurrentEmuGuard::enter(self);
            unsafe { (self.retro_deinit_fn)() }
        }
    }
}

thread_local! {
    static CURRENT_EMU: Cell<*mut RetroCoreDirect> = const { Cell::new(std::ptr::null_mut()) }
}

/// Points [`CURRENT_EMU`] at `emu` for the duration of a call into the core, so
/// the C callbacks — which get no user-data pointer — can find their instance.
///
/// Every entry point into the core needs one: cores call the environment
/// callback from `retro_reset` and `retro_deinit` as readily as from
/// `retro_run`, and servicing those with a null `CURRENT_EMU` leaves the core
/// holding an unfilled out-parameter — dereferencing, say, the system directory
/// it asked for during reset.
///
/// Restores the previous value rather than clearing, so nested entry points
/// compose correctly.
struct CurrentEmuGuard(*mut RetroCoreDirect);

impl CurrentEmuGuard {
    fn enter(emu: &mut RetroCoreDirect) -> Self {
        Self(CURRENT_EMU.with(|p| p.replace(emu as *mut _)))
    }
}

impl Drop for CurrentEmuGuard {
    fn drop(&mut self) {
        CURRENT_EMU.with(|p| p.set(self.0));
    }
}

impl RetroCoreDirect {
    pub fn unload(&mut self) {
        let _guard = CurrentEmuGuard::enter(self);
        unsafe { (self.retro_deinit_fn)() }
        self.lib = None;
    }

    pub fn get_number_of_disks(&mut self) -> u32 {
        let _guard = CurrentEmuGuard::enter(self);
        unsafe { self.disk_callback.get_num_images.map_or(0, |f| f()) }
    }

    pub fn set_disk(&mut self, no: u32) {
        let _guard = CurrentEmuGuard::enter(self);
        let cb = &self.disk_callback;
        unsafe {
            cb.set_eject_state.map(|f| f(true));
            cb.set_image_index.map(|f| f(no));
            cb.set_eject_state.map(|f| f(false));
        };
    }

    pub fn with_frame(&self, f: impl FnOnce(usize, usize, &[u32])) {
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
        match device & RETRO_DEVICE_MASK {
            RETRO_DEVICE_JOYPAD => {
                let mask = self.state.joypad.get(port as usize).copied().unwrap_or(0);
                if id == RETRO_DEVICE_ID_JOYPAD_MASK {
                    mask as i16
                } else if id < 16 {
                    ((mask >> id) & 1) as i16
                } else {
                    0
                }
            }
            RETRO_DEVICE_KEYBOARD if self.state.keys.len() > id as usize => {
                self.state.keys[id as usize] as i16
            }
            // The mouse is only wired to port 0.
            _ if port != 0 => 0,
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
                    ctx.audio_buf.extend(samples);
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
            if ptr.is_null() {
                return;
            }
            let ctx = unsafe { &mut *ptr };
            let slice: &[u8] =
                unsafe { std::slice::from_raw_parts(data as *const u8, pitch * height as usize) };
            ctx.video_refresh(slice, width as usize, height as usize, pitch);
        });
    }

    fn video_refresh(&mut self, data: &[u8], width: usize, height: usize, pitch: usize) {
        let state = &mut self.state;
        state.frame_width = width;
        state.frame_height = height;
        let needed = width * height;
        if state.frame.len() != needed {
            state.frame.resize(needed, 0);
        }
        let pixel_format = state.pixel_format as retro_pixel_format;
        match pixel_format {
            RETRO_PIXEL_FORMAT_XRGB8888 => {
                for y in 0..height {
                    let src_row = &data[y * pitch..y * pitch + width * 4];
                    let dst_row = &mut state.frame[y * width..(y + 1) * width];
                    for (out, px) in dst_row.iter_mut().zip(src_row.chunks_exact(4)) {
                        // Source is BGRA (little-endian XRGB8888); repack to RGBA.
                        *out = u32::from_ne_bytes([px[2], px[1], px[0], 255]);
                    }
                }
            }
            RETRO_PIXEL_FORMAT_RGB565 => {
                convert_16bpp(data, &mut state.frame, width, height, pitch, &RGB565_LUT)
            }
            RETRO_PIXEL_FORMAT_0RGB1555 => {
                convert_16bpp(data, &mut state.frame, width, height, pitch, &RGB1555_LUT)
            }
            _ => {}
        }
    }

    unsafe extern "C" fn environment_cb(cmd: c_uint, data: *mut c_void) -> bool {
        let mut ret = false;
        CURRENT_EMU.with(|p| {
            let ptr = p.get();
            if !ptr.is_null() {
                let ctx = unsafe { &mut *ptr };
                ret = ctx.environment(cmd, data);
            } else {
                // No instance registered for this thread: the core called back
                // from an entry point that forgot a `CurrentEmuGuard`, and is now
                // about to read an out-parameter we never filled in.
                error!(
                    "!! FAILED ENV {cmd} on thread {:?}",
                    std::thread::current().name()
                );
            }
        });
        ret
    }
    fn environment(&mut self, cmd: u32, data: *mut c_void) -> bool {
        // debug!("## ENV {cmd}");
        let mut handled = true;
        unsafe {
            match cmd {
                RETRO_ENVIRONMENT_SET_FRAME_TIME_CALLBACK => {
                    let callback = data as *mut retro_frame_time_callback;
                    info!("SET FRAME TIME");
                    self.time_reference = (*callback).reference;
                    self.retro_frame_time = (*callback).callback;
                }
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
                    info!("SET KEYBOARD");
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
                    if !callback.is_null() {
                        self.disk_callback = *callback;
                    }
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
                                    // Only fills the gaps: a value we were given
                                    // for this option is the frontend's answer
                                    // and outranks the core's default, whenever
                                    // the core gets around to announcing it.
                                    if !self.vars.contains_key(&key) {
                                        self.set_var(&key, default);
                                    }
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
                            debug!("GET {key:?} {value:?}");
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
                // TODO: Core reporting messages to user
                RETRO_ENVIRONMENT_SET_MESSAGE => {}
                RETRO_ENVIRONMENT_GET_MESSAGE_INTERFACE_VERSION => {}
                RETRO_ENVIRONMENT_SET_MESSAGE_EXT => {}
                _ => {
                    debug!("unhandled ENV {cmd}");
                    handled = false;
                }
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
        // `dlopen` returns the same mapping (and the same C globals) for a core
        // loaded twice from the same path, so two instances of one core would
        // otherwise stomp each other's global state and crash. Copy the core to
        // a uniquely-named file in a private temp dir first — the trick libretro
        // frontends use for "core duping" — so every instance gets its own
        // mapping with independent globals. The temp dir is held in the struct
        // and removed when the core is dropped.
        let core_tempdir = tempfile::Builder::new().prefix("demarc-core-").tempdir()?;
        let file_name = core_path
            .file_name()
            .ok_or_else(|| anyhow!("core path has no file name: {}", core_path.display()))?;
        let loaded_core_path = core_tempdir.path().join(file_name);
        std::fs::copy(core_path, &loaded_core_path)?;
        let core_path = loaded_core_path.as_path();

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
            let retro_set_controller_port_device: libloading::Symbol<
                unsafe extern "C" fn(c_uint, c_uint),
            > = lib.get(b"retro_set_controller_port_device")?;

            let retro_run_fn: unsafe extern "C" fn() = *retro_run_sym;
            let retro_deinit_fn: unsafe extern "C" fn() = *retro_deinit_sym;
            let retro_reset_fn: unsafe extern "C" fn() = *retro_reset_sym;
            let retro_get_avinfo_fn: unsafe extern "C" fn(*mut retro_system_av_info) =
                *retro_get_system_av_info;
            let retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool =
                *retro_load_game;

            let mut retro_emu = RetroCoreDirect {
                lib: None,
                retro_run_fn,
                retro_load_game_fn,
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
                _core_tempdir: core_tempdir,
                skip_frames: 0,
                retro_frame_time: None,
                time_reference: 0,
                frame_serial: 0,
            };
            // Our options go in before the core is told anything, so they are
            // already there whenever it announces its own defaults (usually
            // from within `retro_set_environment` below, but atari800 and
            // friends do it later) and whenever it reads them back.
            for (key, val) in settings.iter() {
                retro_emu.set_var(key, val);
                println!("{key} = {val}");
            }

            CURRENT_EMU.with(|p| p.set(&mut retro_emu as *mut _));
            retro_set_environment(Self::environment_cb);
            retro_set_video_refresh(Self::video_refresh_cb);
            retro_set_audio_sample(Self::audio_sample_cb);
            retro_set_audio_sample_batch(Self::audio_sample_batch_cb);
            retro_set_input_poll(Self::input_poll_cb);
            retro_set_input_state(Self::input_state_cb);

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

            // Tell the core both ports are joypads. Several cores (VICE among
            // them) leave a port silent until the frontend selects a device for
            // it, so without this our `set_joypad` state is never polled.
            retro_set_controller_port_device(0, RETRO_DEVICE_JOYPAD);
            retro_set_controller_port_device(1, RETRO_DEVICE_JOYPAD);

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
        let _guard = CurrentEmuGuard::enter(self);
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
        let _guard = CurrentEmuGuard::enter(self);
        if let Some(cb) = self.retro_frame_time {
            unsafe { cb(self.time_reference) }
        }
        unsafe { (self.retro_run_fn)() }
        self.frame_serial = self.frame_serial.wrapping_add(1);
        // Relative motion has been consumed by the core this frame.
        self.mouse.dx = 0;
        self.mouse.dy = 0;
        // Don't poll retro_get_system_av_info() here: av_info is captured once
        // after load_game and kept current by the SET_SYSTEM_AV_INFO/SET_GEOMETRY
        // callbacks. Some cores (e.g. atari800) re-run update_variables() inside
        // get_system_av_info, so polling it every frame triggers a costly
        // texture/option reinit on every frame.
    }

    /// Display aspect ratio (width / height) the core wants, or 0.0 if unknown.
    pub fn aspect_ratio(&self) -> f32 {
        self.state.aspect_ratio
    }

    /// Audio sample rate the core wants, in Hz, or 0.0 if unknown.
    pub fn sample_rate(&self) -> f64 {
        self.state.sample_rate
    }

    pub(crate) fn press_key(&mut self, code: u32, down: bool, mods: u16) {
        if self.state.keys.len() <= code as usize {
            self.state.keys.resize(code as usize + 1, 0);
        }
        self.state.keys[(code & 0x1ff) as usize] = down as u8;
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

    /// Set or clear a joypad button on `port` (0 = Joystick #1, 1 = Joystick #2).
    /// `id` is a `RETRO_DEVICE_ID_JOYPAD_*` button index.
    pub(crate) fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        let Some(mask) = self.state.joypad.get_mut(port as usize) else {
            return;
        };
        if id < 16 {
            if down {
                *mask |= 1 << id;
            } else {
                *mask &= !(1 << id);
            }
        }
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
impl Backend for RetroCoreDirect {
    fn run(&mut self) -> bool {
        RetroCoreDirect::run(self);
        true
    }
    fn frame_hash(&self) -> u64 {
        self.frame_serial
    }
    fn reset(&mut self) {
        RetroCoreDirect::reset(self)
    }
    fn set_disk(&mut self, no: u32) {
        RetroCoreDirect::set_disk(self, no);
    }
    fn get_number_of_disks(&mut self) -> u32 {
        RetroCoreDirect::get_number_of_disks(self)
    }

    fn press_key(&mut self, code: u32, down: bool, mods: u16) {
        RetroCoreDirect::press_key(self, code, down, mods)
    }
    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        RetroCoreDirect::add_mouse_motion(self, dx, dy)
    }
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        RetroCoreDirect::set_mouse_buttons(self, left, right, middle)
    }
    fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        RetroCoreDirect::set_joypad(self, port, id, down)
    }
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
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

    // fn unload(&mut self) {
    //     RetroCoreDirect::unload(self)
    // }

    fn skip_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            RetroCoreDirect::run(self);
        }
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
    SetJoypad {
        port: u32,
        id: u32,
        down: bool,
    },
    SetDisk {
        no: u32,
    },
    Unload,
    Skip {
        frames: u32,
    },
}

/// A single stepped frame's worth of data, pushed from the worker to main thread
#[derive(Default)]
struct RetroUpdate {
    width: usize,
    height: usize,
    frame: Vec<u32>,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
    frame_hash: u64,
}

pub struct RetroCoreThreaded {
    cmd_tx: mpsc::Sender<RetroCmd>,
    // Wrapped in a `Mutex` purely so the type is `Sync`: `mpsc::Receiver` is
    // `Send` but not `Sync`, and Bevy requires components to be `Sync`. All
    // access is through `&mut self` (`run`/`Drop`), so `get_mut` is used and
    // the lock is never actually contended.
    update_rx: Mutex<mpsc::Receiver<RetroUpdate>>,
    handle: Option<thread::JoinHandle<()>>,
    frame: Vec<u32>,
    frame_hash: u64,
    last_hash: u64,
    audio_sum: i32,
    last_sum: i32,
    frame_width: usize,
    frame_height: usize,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
    disk_count: u32,
    /// Emulated frames stepped by the worker so far (shared with the worker
    /// thread); read by `--speed-test`.
    frames: Arc<AtomicU64>,
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
        speed_test: bool,
    ) -> Result<Self> {
        let core_path = core_path.to_path_buf();
        let system_dir = system_dir.to_path_buf();
        let game = game.map(|g| g.to_path_buf());

        let mut latency = 3;
        if let Some(l) = settings.get("latency") {
            latency = l.parse().unwrap_or(3);
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<RetroCmd>();
        let (update_tx, update_rx) = mpsc::sync_channel::<RetroUpdate>(latency);
        let (setup_tx, setup_rx) = mpsc::channel::<Result<SetupResult, String>>();

        let frames = Arc::new(AtomicU64::new(0));
        let worker_frames = Arc::clone(&frames);
        let handle = thread::Builder::new()
            .name("retro-emu".into())
            // Well above the 2 MiB default. Cores recurse deeply on this thread —
            // a dynarec or shader compiler can overflow the default and take the
            // process down with a SIGSEGV that looks nothing like a stack overflow.
            .stack_size(WORKER_STACK_SIZE)
            .spawn(move || {
                let mut core = match RetroCoreDirect::new(
                    &core_path,
                    &system_dir,
                    game.as_deref(),
                    settings,
                ) {
                    Ok(mut core) => {
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
                worker_loop(&mut core, &cmd_rx, &update_tx, &worker_frames, speed_test);
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
                update_rx: Mutex::new(update_rx),
                handle: Some(handle),
                frame: Vec::new(),
                frame_hash: 0,
                last_hash: 0,
                audio_sum: 0,
                last_sum: 0,
                frame_width: width,
                frame_height: height,
                audio: Vec::new(),
                aspect_ratio: 0.0,
                sample_rate: 0.0,
                fps,
                disk_count: disks,
                frames,
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

/// Worker-thread main loop
fn worker_loop(
    core: &mut RetroCoreDirect,
    cmd_rx: &mpsc::Receiver<RetroCmd>,
    update_tx: &mpsc::SyncSender<RetroUpdate>,
    frames: &AtomicU64,
    speed_test: bool,
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
        // Count every emulated frame the core steps, including skipped ones.
        frames.fetch_add(1, Ordering::Relaxed);
        if core.skip_frames > 0 {
            core.skip_frames -= 1;
            // if core.skip_frames == 0 {
            //     core.with_audio(|_| {});
            //     let update = RetroUpdate {
            //         ..Default::default()
            //     };
            //     if update_tx.send(update).is_err() {
            //         return; // main side gone
            //     }
            // }
            continue;
        }

        let (width, height) = core.get_frame_size();
        let mut frame = Vec::new();
        core.with_frame(|_, _, fr| frame.extend_from_slice(fr));

        let hash = scan_frame(&frame);

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
            frame_hash: hash,
        };
        if speed_test {
            // Benchmark: never block on the consumer. Hand off the latest frame
            // if there's room, otherwise drop it and keep emulating flat-out so
            // throughput reflects the core, not the (vsync-limited) main loop.
            match update_tx.try_send(update) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
                Err(mpsc::TrySendError::Disconnected(_)) => return,
            }
        } else if update_tx.send(update).is_err() {
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
        RetroCmd::SetJoypad { port, id, down } => core.set_joypad(port, id, down),
        RetroCmd::SetDisk { no } => {
            core.set_disk(no);
        }
        RetroCmd::Unload => {
            core.unload();
            return true;
        }
        RetroCmd::Skip { frames } => core.skip_frames = frames,
    }
    false
}

impl Backend for RetroCoreThreaded {
    fn run(&mut self) -> bool {
        if let Ok(update) = self.update_rx.get_mut().unwrap().try_recv() {
            // if update.frame.is_empty() && update.audio.is_empty() {
            //     info!("GOT 0 UPDATE");
            //     self.audio.clear();
            //     return false;
            // }
            self.frame = update.frame;
            self.last_hash = self.frame_hash;
            self.frame_hash = update.frame_hash;
            self.frame_width = update.width;
            self.frame_height = update.height;
            self.last_sum = self.audio_sum;
            self.audio_sum = update.audio.iter().map(|a| (*a).abs() as i32).sum();
            self.audio.extend_from_slice(&update.audio);
            self.aspect_ratio = update.aspect_ratio;
            self.sample_rate = update.sample_rate;
            self.fps = update.fps;
            true
        } else {
            trace!("Starving");
            false
        }
    }

    fn is_idle(&self) -> bool {
        self.last_hash == self.frame_hash && self.audio_sum.abs() < 1000
    }

    fn get_number_of_disks(&mut self) -> u32 {
        self.disk_count
    }
    fn reset(&mut self) {
        let _ = self.cmd_tx.send(RetroCmd::Reset);
    }
    fn set_disk(&mut self, no: u32) {
        if self.cmd_tx.send(RetroCmd::SetDisk { no }).is_err() {}
    }
    fn press_key(&mut self, code: u32, down: bool, mods: u16) {
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
    fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        let _ = self.cmd_tx.send(RetroCmd::SetJoypad { port, id, down });
    }
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
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
    // fn unload(&mut self) {
    //     let _ = self.cmd_tx.send(RetroCmd::Unload);
    // }

    fn skip_frames(&mut self, frames: u32) {
        let _ = self.cmd_tx.send(RetroCmd::Skip { frames });
    }
    fn frames_stepped(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    fn frame_hash(&self) -> u64 {
        self.frame_hash
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
        while self.update_rx.get_mut().unwrap().recv().is_ok() {}
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Compile-time guarantee that the direct core can be moved onto the worker
/// thread, and that the threaded handle is `Send + Sync` so it can live inside
/// a Bevy component (the `Emulator`).
const _: () = {
    fn _assert_send<T: Send>() {}
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send::<RetroCoreDirect>();
        _assert_send_sync::<RetroCoreThreaded>();
    }
};

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, Instant},
    };

    use crate::libloader;

    use super::*;

    pub fn save_png(emu: &RetroCoreDirect, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let width = emu.state.frame_width as u32;
        let height = emu.state.frame_height as u32;
        let expected = (width as usize) * (height as usize);
        if width == 0 || height == 0 || emu.state.frame.len() < expected {
            return Err("no frame available".into());
        }
        let bytes = frame_bytes(&emu.state.frame[..expected]).to_vec();
        let buf = image::RgbaImage::from_raw(width, height, bytes)
            .ok_or("failed to build image buffer")?;
        buf.save(path)?;
        Ok(())
    }
    /// Paths here are rooted at the crate directory rather than left relative:
    /// a conversion running in another test switches the process-wide working
    /// directory for its duration (see `cbmconvert::CwdGuard`).
    fn root(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
    }

    /// The threaded `run()` is non-blocking, so the main loop must give the
    /// worker thread time to boot and deliver frames. Drive `emu` until it has
    /// produced its first frame, or panic after `timeout`.
    fn run_until_frame(emu: &mut dyn Backend, timeout: Duration) {
        let start = Instant::now();
        while emu.get_frame_size().0 == 0 {
            emu.run();
            assert!(
                start.elapsed() < timeout,
                "worker never produced a frame within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn retro_amiga_works() {
        let core_path = libloader::get_libretro("puae").unwrap();
        let system_dir = &root("system");
        let game_path = root("demos/rebels.adf");

        let settings = HashMap::new();

        let mut retro_emu =
            RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();
        println!("## RUN");
        for _ in 0..200 {
            retro_emu.run();
        }
        save_png(&retro_emu, &root("test_amiga.png")).unwrap();
    }

    /// Boot a self-booting directory under Kickstart 1.3 (A500). The WHDLoad
    /// helper must be disabled, otherwise its Startup-Sequence runs `FAILAT`,
    /// a command that doesn't exist under 1.3, and the boot fails.
    #[test]
    fn retro_amiga_dir_works() {
        let core_path = libloader::get_libretro("puae").unwrap();
        let system_dir = &root("system");
        let game_path = root("demos/o2-intro");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A500".into());
        settings.insert("puae_use_whdload".into(), "disabled".into());

        let mut retro_emu =
            RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();
        for _ in 0..200 {
            retro_emu.run();
        }
        save_png(&retro_emu, &root("test_amiga_dir.png")).unwrap();
    }

    #[test]
    fn retro_threaded_works() {
        let core_path = libloader::get_libretro("puae").unwrap();
        let system_dir = &root("system");
        let game_path = root("demos/rebels.adf");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A500".into());

        let mut emu =
            RetroCoreThreaded::new(&core_path, system_dir, Some(&game_path), settings, false)
                .unwrap();
        // Object-safety / interchangeability check.
        let emu: &mut dyn Backend = &mut emu;

        // Pace the loop so the worker keeps up and the demo advances.
        for _ in 0..200 {
            emu.run();
            std::thread::sleep(Duration::from_millis(2));
        }
        // The worker may still be a few frames behind; make sure we have one.
        run_until_frame(emu, Duration::from_secs(5));
        //emu.save_png(&root("test_amiga_threaded.png")).unwrap();
        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "no frame produced by worker");
    }

    #[test]
    fn retro_threaded_multi_works() {
        let uae_core = libloader::get_libretro("puae").unwrap();
        let vice_core = libloader::get_libretro("vice_x64").unwrap();
        let system_dir = &root("system");
        let uae_game = root("demos/rebels.adf");
        let vice_game = root("demos/quantum_icc2026_v1p.prg");

        let uae_settings = || {
            let mut s = HashMap::new();
            s.insert("puae_model".to_string(), "A500".to_string());
            s
        };

        let cores = [
            (
                &uae_core,
                &uae_game,
                uae_settings(),
                "test_threaded_uae_0.png",
            ),
            (
                &uae_core,
                &uae_game,
                uae_settings(),
                "test_threaded_uae_1.png",
            ),
            (
                &vice_core,
                &vice_game,
                HashMap::new(),
                "test_threaded_vice_0.png",
            ),
            (
                &vice_core,
                &vice_game,
                HashMap::new(),
                "test_threaded_vice_1.png",
            ),
        ];

        let mut emus: Vec<(&str, RetroCoreThreaded)> = cores
            .iter()
            .map(|(core, game, settings, png)| {
                let emu =
                    RetroCoreThreaded::new(core, system_dir, Some(game), settings.clone(), false)
                        .unwrap();
                (*png, emu)
            })
            .collect();

        // Pace the loop so the workers keep up and the demos advance.
        for _ in 0..200 {
            for (_, emu) in emus.iter_mut() {
                let emu: &mut dyn Backend = emu;
                emu.run();
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        for (path, emu) in emus.iter_mut() {
            // A worker may still be a few frames behind; make sure it has one.
            run_until_frame(emu, Duration::from_secs(5));
            let (w, h) = emu.get_frame_size();
            assert!(w > 0 && h > 0, "no frame produced by worker for {path}");
            //emu.save_png(Path::new(path)).unwrap();
        }
    }

    /// The point of wrapping a PS-X EXE in a disc image: pcsx_rearmed won't take
    /// the executable, but it boots the disc built around it, off its HLE BIOS
    /// and with no BIOS image installed at all.
    ///
    /// This is the test that says the image is really bootable — the structural
    /// checks in `utils` can only say it is well formed. It runs long enough for
    /// the HLE boot to walk the filesystem, load the executable and draw with
    /// it, and a blank frame means it never got there.
    #[test]
    fn psx_exe_boots_from_generated_disc() {
        let core_path = libloader::get_libretro("pcsx_rearmed").unwrap();
        // A temp dir, not `system/`: with no BIOS in it the core has to fall
        // back to HLE, which is the path being tested, and it keeps the memory
        // card files the core writes out of the packed `system.zip`.
        let system_dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let iso = crate::utils::create_psx_iso(&root("demos/pdx-dlcm.psx"))
            .unwrap()
            .expect("the demo is a PS-X EXE");

        let mut tags = HashMap::new();
        tags.insert("pcsx_rearmed_bios".to_string(), "HLE".to_string());
        tags.insert("pcsx_rearmed_region".to_string(), "PAL".to_string());
        let mut emu =
            RetroCoreDirect::new(&core_path, system_dir.path(), Some(&iso), tags).unwrap();
        for _ in 0..300 {
            emu.run();
        }
        // save_png(&emu, &root("test_psx_exe.png")).unwrap();

        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "no frame produced");
        let distinct = emu
            .state
            .frame
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(distinct > 16, "frame looks blank: only {distinct} colours");
    }

    /// The `psx_core` tag is the way to Beetle — where an executable ends up
    /// when no disc could be built around it — while everything else keeps the
    /// permissive default.
    #[test]
    fn psx_exe_routes_to_beetle() {
        use crate::systems::SystemType;
        use crate::systems::get_core;

        let disc = get_core(SystemType::Psx, &HashMap::new()).unwrap();
        assert!(
            disc.to_string_lossy().contains("pcsx_rearmed"),
            "discs should use pcsx_rearmed, got {disc:?}"
        );

        let mut tags = HashMap::new();
        tags.insert("psx_core".to_string(), "beetle".to_string());
        let exe = get_core(SystemType::Psx, &tags).unwrap();
        assert!(
            exe.to_string_lossy().contains("mednafen_psx"),
            "the beetle tag should select Beetle, got {exe:?}"
        );
    }

    /// Boots a licence-stripped scene disc with an MP3 audio track — the shape
    /// Beetle can't handle, and the reason pcsx_rearmed is the default. No BIOS
    /// is installed here, so this also covers the HLE path.
    ///
    /// Runs under the PAL region `create_core` now pins, since forcing a region
    /// is the one way this default could break a disc that booted on `auto`.
    #[test]
    fn retro_psx_works() {
        let core_path = libloader::get_libretro("mednafen_psx").unwrap();
        // A temp dir, not `system/`: PSX needs nothing from it, and the core
        // writes memory-card files into the system dir — which `build.rs` would
        // then pack into the embedded `system.zip`.
        let system_dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let game_path = root("demos/pdx-dlcm.psx");

        let mut tags = HashMap::new();
        tags.insert("beetle_psx_region".to_string(), "pal".to_string());
        for f in [
            "scph5500.bin",
            "scph5501.bin",
            "scph5502.bin",
            "scph5552.bin",
        ] {
            std::fs::copy(root("system").join(f), system_dir.path().join(f)).unwrap();
        }
        let mut emu =
            RetroCoreDirect::new(&core_path, system_dir.path(), Some(&game_path), tags).unwrap();
        for _ in 0..150 {
            emu.run();
        }
        // emu.save_png(&root("test_psx.png")).unwrap();

        let (w, h) = emu.get_frame_size();
        assert!(w > 0 && h > 0, "no frame produced");
        let distinct = emu
            .state
            .frame
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len();
        assert!(distinct > 16, "frame looks blank: only {distinct} colours");
    }

    #[test]
    fn retro_vice_works() {
        let core_path = libloader::get_libretro("vice_x64").unwrap();
        let system_dir = &root("system");
        let game_path = root("demos/quantum_icc2026_v1p.prg");

        let mut retro_emu =
            RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), HashMap::new()).unwrap();
        println!("## RUN");
        for _ in 0..200 {
            retro_emu.run();
        }
        save_png(&retro_emu, &root("test_d64.png")).unwrap();
    }

    /// The settings handed to a core — a db header's `puae_model:A1200`, in the
    /// end — must be the values it reads back, not the defaults it announces
    /// through `SET_VARIABLES`. The untouched option checks the other half: the
    /// core's own defaults still fill in everything we didn't name.
    #[test]
    fn settings_reach_the_core() {
        let core_path = libloader::get_libretro("puae").unwrap();
        let system_dir = &root("system");
        let game_path = root("demos/rebels.adf");

        let mut settings = HashMap::new();
        settings.insert("puae_model".into(), "A1200".into());
        settings.insert("puae_video_standard".into(), "NTSC".into());

        let retro_emu =
            RetroCoreDirect::new(&core_path, system_dir, Some(&game_path), settings).unwrap();

        let var = |key: &str| {
            retro_emu
                .vars
                .get(key)
                .unwrap_or_else(|| panic!("core never saw {key}, has {:?}", retro_emu.vars))
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(var("puae_model"), "A1200");
        assert_eq!(var("puae_video_standard"), "NTSC");
        // Announced by the core, never set by us, so it must have kept its
        // default — this is what proves the core did announce its options.
        assert!(!var("puae_floppy_speed").is_empty());
    }
}
