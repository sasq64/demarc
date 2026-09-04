use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_ushort, c_void};
use std::path::Path;

use anyhow::{Result, anyhow};
use tracing::{debug, error, info, trace, warn};

use libloading::Library;

use crate::backend::{Backend, ViewFocus};

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
    RETRO_ENVIRONMENT_GET_CURRENT_SOFTWARE_FRAMEBUFFER, RETRO_ENVIRONMENT_GET_FASTFORWARDING,
    RETRO_ENVIRONMENT_GET_INPUT_BITMASKS, RETRO_ENVIRONMENT_GET_LANGUAGE,
    RETRO_ENVIRONMENT_GET_LIBRETRO_PATH, RETRO_ENVIRONMENT_GET_LOG_INTERFACE,
    RETRO_ENVIRONMENT_GET_MESSAGE_INTERFACE_VERSION, RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY,
    RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY, RETRO_ENVIRONMENT_GET_THROTTLE_STATE,
    RETRO_ENVIRONMENT_GET_VARIABLE, RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE,
    RETRO_ENVIRONMENT_GET_VFS_INTERFACE, RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY,
    RETRO_ENVIRONMENT_SET_DISK_CONTROL_EXT_INTERFACE, RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE,
    RETRO_ENVIRONMENT_SET_FRAME_TIME_CALLBACK, RETRO_ENVIRONMENT_SET_GEOMETRY,
    RETRO_ENVIRONMENT_SET_KEYBOARD_CALLBACK, RETRO_ENVIRONMENT_SET_MESSAGE,
    RETRO_ENVIRONMENT_SET_MESSAGE_EXT, RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL,
    RETRO_ENVIRONMENT_SET_PIXEL_FORMAT, RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO,
    RETRO_ENVIRONMENT_SET_VARIABLES, RETRO_PIXEL_FORMAT_0RGB1555, RETRO_PIXEL_FORMAT_RGB565,
    RETRO_PIXEL_FORMAT_XRGB8888, RETRO_THROTTLE_FAST_FORWARD, RETRO_THROTTLE_NONE,
    retro_audio_sample_batch_t, retro_audio_sample_t, retro_disk_control_callback,
    retro_disk_control_ext_callback, retro_environment_t, retro_frame_time_callback,
    retro_game_geometry, retro_game_info, retro_input_poll_t, retro_input_state_t,
    retro_keyboard_callback, retro_log_callback, retro_log_level, retro_pixel_format,
    retro_system_av_info, retro_throttle_state, retro_variable, retro_vfs_interface_info,
    retro_video_refresh_t,
};
use crate::pixels::{RGB565_LUT, RGB1555_LUT, convert_16bpp, convert_xrgb8888};

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

pub struct RetroCoreDirect {
    lib: Option<Library>,
    retro_run_fn: unsafe extern "C" fn(),
    retro_load_game_fn: unsafe extern "C" fn(*const retro_game_info) -> bool,
    retro_deinit_fn: unsafe extern "C" fn(),
    retro_unload_game_fn: unsafe extern "C" fn(),
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
    visible: bool,
}
impl Drop for RetroCoreDirect {
    fn drop(&mut self) {
        if self.lib.is_some() {
            self.shut_down();
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
    /// Give the content back and shut the core down, in that order.
    ///
    /// Both halves matter, and `retro_unload_game` most of all: a core that
    /// runs its emulation on a thread of its own only stops that thread here.
    /// DOSBox Pure is one — its `retro_deinit` frees a couple of buffers and
    /// nothing else, so skipping the unload leaves the DOS thread running, and
    /// the `dlclose` below then pulls the code out from under it. That is a
    /// SIGSEGV in a thread with no Rust frames in it at all.
    ///
    /// Called on the thread that called `retro_run`, which is what cores that
    /// hand work to another thread expect: the shutdown handshake is with the
    /// frontend thread they have been synchronising with all along.
    fn shut_down(&mut self) {
        let _guard = CurrentEmuGuard::enter(self);
        unsafe {
            (self.retro_unload_game_fn)();
            (self.retro_deinit_fn)();
        }
    }

    /// Shut the core down and unload the library.
    ///
    /// Idempotent: `lib` is taken, and [`Drop`] checks it, so a core that has
    /// been unloaded is not shut down twice.
    pub fn unload(&mut self) {
        self.shut_down();
        // Only now, with the core's own threads stopped, is it safe to unmap
        // the code they were running.
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
                convert_xrgb8888(data, &mut state.frame, width, height, pitch)
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
                    info!(
                        "Got GEOMETRY {}x{} ASPECT {}",
                        geom.base_width, geom.base_height, self.state.aspect_ratio
                    );
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
                RETRO_ENVIRONMENT_GET_VFS_INTERFACE => {
                    // Not a nicety: modern Stella refuses to load *any* ROM
                    // without a VFS, because its FSNode only learns a path is a
                    // file from the VFS stat(). See src/retro_emu/vfs.rs.
                    let info = &mut *(data as *mut retro_vfs_interface_info);
                    if info.required_interface_version > vfs::VERSION {
                        info!(
                            "Core wants VFS v{}, we provide v{}",
                            info.required_interface_version,
                            vfs::VERSION
                        );
                        handled = false;
                    } else {
                        info!("VFS v{} registered", vfs::VERSION);
                        // The frontend reports back the version it actually
                        // implements, which may be newer than what was asked.
                        info.required_interface_version = vfs::VERSION;
                        info.iface = vfs::interface();
                    }
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
                    //debug!("{:?}", self.vars);
                }
                RETRO_ENVIRONMENT_GET_VARIABLE => {
                    let var = &mut *(data as *mut retro_variable);
                    if !var.key.is_null() {
                        let key = CStr::from_ptr(var.key).to_string_lossy();
                        if let Some(value) = self.vars.get(key.as_ref()) {
                            trace!("GET {key:?} {value:?}");
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
                // Ignore option display hints
                RETRO_ENVIRONMENT_SET_CORE_OPTIONS_DISPLAY => {}
                RETRO_ENVIRONMENT_GET_FASTFORWARDING => {
                    *(data as *mut c_uint) = if self.skip_frames > 0 { 1 } else { 0 };
                }
                RETRO_ENVIRONMENT_GET_CURRENT_SOFTWARE_FRAMEBUFFER => {
                    // TODO: Return unsafe pointer to frame?
                }
                RETRO_ENVIRONMENT_SET_PERFORMANCE_LEVEL => {}
                RETRO_ENVIRONMENT_GET_THROTTLE_STATE => {
                    let state = data as *mut retro_throttle_state;
                    (*state).rate = 1.0;
                    (*state).mode = if self.skip_frames > 0 {
                        RETRO_THROTTLE_FAST_FORWARD
                    } else {
                        RETRO_THROTTLE_NONE
                    };
                }
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
            let retro_unload_game_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_unload_game")?;
            let retro_reset_sym: libloading::Symbol<unsafe extern "C" fn()> =
                lib.get(b"retro_reset")?;
            let retro_set_controller_port_device: libloading::Symbol<
                unsafe extern "C" fn(c_uint, c_uint),
            > = lib.get(b"retro_set_controller_port_device")?;

            let retro_run_fn: unsafe extern "C" fn() = *retro_run_sym;
            let retro_deinit_fn: unsafe extern "C" fn() = *retro_deinit_sym;
            let retro_unload_game_fn: unsafe extern "C" fn() = *retro_unload_game_sym;
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
                retro_unload_game_fn,
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
                visible: true,
            };
            // Our options go in before the core is told anything, so they are
            // already there whenever it announces its own defaults (usually
            // from within `retro_set_environment` below, but atari800 and
            // friends do it later) and whenever it reads them back.
            for (key, val) in settings.iter() {
                retro_emu.set_var(key, val);
            }

            CURRENT_EMU.with(|p| p.set(&mut retro_emu as *mut _));
            retro_set_environment(Self::environment_cb);
            retro_set_video_refresh(Self::video_refresh_cb);
            retro_set_audio_sample(Self::audio_sample_cb);
            retro_set_audio_sample_batch(Self::audio_sample_batch_cb);
            retro_set_input_poll(Self::input_poll_cb);
            retro_set_input_state(Self::input_state_cb);

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
            info!("Got avinfo: {:?}", av_info);

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
        // Windows canonicalize() adds \\?\ (extended-length path prefix) which most
        // C libraries including libretro cores don't understand — strip it.
        let abs_path = crate::utils::strip_verbatim_prefix(&abs_path);
        let path_str = abs_path.to_string_lossy();
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
        // if !self.visible {
        //     return;
        // }
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
    fn focus(&mut self, focus: ViewFocus) {
        self.visible = focus != ViewFocus::Invisible
    }

    fn skip_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            RetroCoreDirect::run(self);
        }
    }
}

mod threaded;
pub use threaded::RetroCoreThreaded;

mod vfs;

#[cfg(test)]
#[path = "tests/retro_emu_tests.rs"]
mod tests;
