//! Runs a libretro core in a child process (this same binary, started with
//! [`WORKER_ARG`]), so a crashing core only takes itself down.
//!
//! Commands and frame hand-offs go over a socket pair as fixed-size messages.
//! Pixels and audio live in shared memory split into slots: the core converts
//! each frame straight into a slot the child owns, the child hands the slot
//! over, and the parent reads it in place until it passes the slot back.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{error, trace};
use tracing_subscriber::EnvFilter;

use crate::backend::{Backend, STATE_SKIPPING, ViewFocus};
use crate::pixels::{SCREEN_ACTIVE, get_frame_diff};

use super::threaded::{RetroCmd, WORKER_STACK_SIZE, apply_cmd, set_state_bit};
use super::{FrameTarget, RetroCoreDirect};

const WORKER_ARG: &str = "--retro-process-worker";

/// Largest frame a slot can hold. Pages are only committed once touched.
const MAX_PIXELS: usize = 4096 * 4096;
/// Most audio samples (both channels counted) a slot carries per frame.
const MAX_AUDIO: usize = 1 << 20;
const ERROR_LEN: usize = 4096;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[repr(C)]
struct ShmHeader {
    frames: AtomicU64,
    state: AtomicU64,
    fps: f64,
    width: u64,
    height: u64,
    used_width: u64,
    used_height: u64,
    disks: u64,
    error_len: u64,
    error: [u8; ERROR_LEN],
}

#[repr(C)]
struct SlotHeader {
    width: u64,
    height: u64,
    used_width: u64,
    used_height: u64,
    frame_diff: f32,
    aggregated_diff: f32,
    sample_rate: f64,
    fps: f64,
    aspect_ratio: f32,
    audio_len: u32,
}

const fn align8(n: usize) -> usize {
    (n + 7) & !7
}

const HEADER_SIZE: usize = align8(size_of::<ShmHeader>());
const SLOT_HEADER_SIZE: usize = align8(size_of::<SlotHeader>());
const SLOT_SIZE: usize = align8(SLOT_HEADER_SIZE + MAX_PIXELS * 4 + MAX_AUDIO * 2);

/// The shared mapping, header first and then `slots` slots.
struct Shm {
    ptr: *mut u8,
    len: usize,
    slots: usize,
}

// Every access goes through the ownership handshake described in the module docs.
unsafe impl Send for Shm {}
unsafe impl Sync for Shm {}

impl Shm {
    fn map(file: &File, slots: usize) -> io::Result<Self> {
        let len = HEADER_SIZE + slots * SLOT_SIZE;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr.cast(),
            len,
            slots,
        })
    }

    fn header(&self) -> *mut ShmHeader {
        self.ptr.cast()
    }

    fn slot(&self, slot: usize) -> *mut SlotHeader {
        assert!(slot < self.slots);
        unsafe { self.ptr.add(HEADER_SIZE + slot * SLOT_SIZE).cast() }
    }

    fn pixels(&self, slot: usize) -> *mut u32 {
        unsafe { self.slot(slot).cast::<u8>().add(SLOT_HEADER_SIZE).cast() }
    }

    fn audio(&self, slot: usize) -> *mut i16 {
        unsafe { self.pixels(slot).cast::<u8>().add(MAX_PIXELS * 4).cast() }
    }

    fn atomics(&self) -> (&AtomicU64, &AtomicU64) {
        let h = self.header();
        unsafe { (&(*h).frames, &(*h).state) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

#[cfg(target_os = "linux")]
fn shared_file() -> io::Result<File> {
    let fd = unsafe { libc::memfd_create(c"demarc-core".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(not(target_os = "linux"))]
fn shared_file() -> io::Result<File> {
    tempfile::tempfile()
}

// Parent to child
const MSG_RESET: u32 = 1;
const MSG_PRESS_KEY: u32 = 2;
const MSG_MOUSE_MOTION: u32 = 3;
const MSG_MOUSE_BUTTONS: u32 = 4;
const MSG_JOYPAD: u32 = 5;
const MSG_SET_DISK: u32 = 6;
const MSG_UNLOAD: u32 = 7;
const MSG_SKIP: u32 = 8;
const MSG_FOCUS: u32 = 9;
/// Followed by `a` [`MSG_KEY`] messages.
const MSG_SEND_KEYS: u32 = 10;
const MSG_KEY: u32 = 11;
const MSG_RELEASE: u32 = 12;
// Child to parent
const MSG_SETUP_OK: u32 = 20;
const MSG_SETUP_ERR: u32 = 21;
const MSG_FRAME: u32 = 22;

const MSG_SIZE: usize = 24;

#[derive(Clone, Copy, Default)]
struct Msg {
    tag: u32,
    a: u32,
    b: u32,
    c: u32,
    x: f32,
    y: f32,
}

impl Msg {
    fn new(tag: u32, a: u32, b: u32, c: u32) -> Self {
        Self {
            tag,
            a,
            b,
            c,
            ..Default::default()
        }
    }

    fn encode(&self) -> [u8; MSG_SIZE] {
        let mut out = [0; MSG_SIZE];
        let fields = [
            self.tag,
            self.a,
            self.b,
            self.c,
            self.x.to_bits(),
            self.y.to_bits(),
        ];
        for (chunk, field) in out.chunks_exact_mut(4).zip(fields) {
            chunk.copy_from_slice(&field.to_ne_bytes());
        }
        out
    }

    fn decode(bytes: &[u8]) -> Self {
        let field = |i: usize| u32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
        Self {
            tag: field(0),
            a: field(1),
            b: field(2),
            c: field(3),
            x: f32::from_bits(field(4)),
            y: f32::from_bits(field(5)),
        }
    }

    fn to_cmd(self) -> Option<RetroCmd> {
        Some(match self.tag {
            MSG_RESET => RetroCmd::Reset,
            MSG_PRESS_KEY => RetroCmd::PressKey {
                code: self.a,
                down: self.b != 0,
                mods: self.c as u16,
            },
            MSG_MOUSE_MOTION => RetroCmd::AddMouseMotion {
                dx: self.x,
                dy: self.y,
            },
            MSG_MOUSE_BUTTONS => RetroCmd::SetMouseButtons {
                left: self.a != 0,
                right: self.b != 0,
                middle: self.c != 0,
            },
            MSG_JOYPAD => RetroCmd::SetJoypad {
                port: self.a,
                id: self.b,
                down: self.c != 0,
            },
            MSG_SET_DISK => RetroCmd::SetDisk { no: self.a },
            MSG_UNLOAD => RetroCmd::Unload,
            MSG_SKIP => RetroCmd::Skip { frames: self.a },
            MSG_FOCUS => RetroCmd::SetFocus {
                focus: match self.a {
                    0 => ViewFocus::Invisible,
                    1 => ViewFocus::Visible,
                    _ => ViewFocus::Focus,
                },
            },
            _ => return None,
        })
    }
}

struct Channel {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Channel {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    fn send(&mut self, msg: Msg) -> io::Result<()> {
        self.stream.write_all(&msg.encode())
    }

    /// Next message, or `None` if `block` is false and none has arrived yet.
    /// A closed peer is an error.
    fn recv(&mut self, block: bool) -> io::Result<Option<Msg>> {
        while self.buf.len() < MSG_SIZE {
            let mut tmp = [0u8; 4096];
            let flags = if block { 0 } else { libc::MSG_DONTWAIT };
            let n = unsafe {
                libc::recv(
                    self.stream.as_raw_fd(),
                    tmp.as_mut_ptr().cast(),
                    tmp.len(),
                    flags,
                )
            };
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            if n < 0 {
                let err = io::Error::last_os_error();
                match err.kind() {
                    io::ErrorKind::Interrupted => continue,
                    io::ErrorKind::WouldBlock => return Ok(None),
                    _ => return Err(err),
                }
            }
            self.buf.extend_from_slice(&tmp[..n as usize]);
        }
        let msg = Msg::decode(&self.buf[..MSG_SIZE]);
        self.buf.drain(..MSG_SIZE);
        Ok(Some(msg))
    }

    fn recv_blocking(&mut self) -> io::Result<Msg> {
        self.recv(true)
            .map(|m| m.expect("blocking recv returns a message"))
    }
}

pub struct RetroCoreProcess {
    chan: Channel,
    child: Child,
    shm: Shm,
    /// The slot currently on display, owned by us until the next one arrives.
    slot: Option<usize>,
    dead: bool,
    /// Motion in the last frame handed over, and the moving average of it —
    /// see [`get_frame_diff`]. Together they are [`Self::screen_changed`].
    frame_diff: f32,
    aggregated_diff: f32,
    audio_sum: i32,
    frame_width: usize,
    frame_height: usize,
    used_width: usize,
    used_height: usize,
    audio: Vec<i16>,
    aspect_ratio: f32,
    sample_rate: f64,
    fps: f64,
    disk_count: u32,
    info: Option<String>,
}

impl RetroCoreProcess {
    pub fn new(
        core_path: &Path,
        system_dir: &Path,
        game: Option<&Path>,
        meta: HashMap<String, String>,
        speed_test: bool,
    ) -> Result<Self> {
        let latency: usize = meta
            .get("latency")
            .and_then(|l| l.parse().ok())
            .unwrap_or(3);
        // One on display, one being rendered, the rest in flight.
        let slots = latency.max(1) + 2;

        let file = shared_file()?;
        file.set_len((HEADER_SIZE + slots * SLOT_SIZE) as u64)?;
        let shm = Shm::map(&file, slots)?;

        let (ours, theirs) = UnixStream::pair()?;
        let inherit = [theirs.as_raw_fd(), file.as_raw_fd()];
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg(WORKER_ARG)
            .arg(inherit[0].to_string())
            .arg(inherit[1].to_string())
            .stdin(Stdio::null());
        unsafe {
            cmd.pre_exec(move || {
                for fd in inherit {
                    if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().context("could not start core process")?;
        drop(theirs);
        drop(file);

        let mut chan = Channel::new(ours);
        let setup = send_setup(
            &mut chan, core_path, system_dir, game, &meta, speed_test, slots,
        )
        .and_then(|()| chan.recv_blocking());
        let msg = match setup {
            Ok(msg) => msg,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("core process failed during setup: {e}");
            }
        };
        let h = unsafe { &*shm.header() };
        if msg.tag != MSG_SETUP_OK {
            let _ = child.wait();
            let len = (h.error_len as usize).min(ERROR_LEN);
            let e = String::from_utf8_lossy(&h.error[..len]);
            return Err(anyhow!("failed to create core: {e}"));
        }
        let (frame_width, frame_height) = (h.width as usize, h.height as usize);
        let (used_width, used_height) = (h.used_width as usize, h.used_height as usize);
        let (fps, disk_count) = (h.fps, h.disks as u32);

        Ok(Self {
            frame_width,
            frame_height,
            used_width,
            used_height,
            fps,
            disk_count,
            chan,
            child,
            shm,
            slot: None,
            dead: false,
            frame_diff: 0.0,
            aggregated_diff: 0.0,
            audio_sum: 0,
            audio: Vec::new(),
            aspect_ratio: 0.0,
            sample_rate: 0.0,
            info: meta.get("info").cloned(),
        })
    }

    fn send(&mut self, msg: Msg) {
        if self.chan.send(msg).is_err() {
            self.report_dead();
        }
    }

    fn report_dead(&mut self) {
        if !self.dead {
            error!("Core process is gone");
            self.dead = true;
        }
    }
}

fn send_setup(
    chan: &mut Channel,
    core_path: &Path,
    system_dir: &Path,
    game: Option<&Path>,
    meta: &HashMap<String, String>,
    speed_test: bool,
    slots: usize,
) -> io::Result<()> {
    let slots = slots.to_string();
    let mut fields: Vec<&[u8]> = vec![
        core_path.as_os_str().as_bytes(),
        system_dir.as_os_str().as_bytes(),
        game.map_or(b"", |g| g.as_os_str().as_bytes()),
        if speed_test { b"1" } else { b"0" },
        slots.as_bytes(),
    ];
    for (key, val) in meta {
        fields.push(key.as_bytes());
        fields.push(val.as_bytes());
    }
    let blob = fields.join(&0u8);
    chan.stream.write_all(&(blob.len() as u32).to_ne_bytes())?;
    chan.stream.write_all(&blob)
}

impl Backend for RetroCoreProcess {
    fn run(&mut self) -> bool {
        let msg = match self.chan.recv(false) {
            Ok(Some(msg)) if msg.tag == MSG_FRAME && (msg.a as usize) < self.shm.slots => msg,
            Ok(_) => {
                trace!("Starving");
                return false;
            }
            Err(_) => {
                self.report_dead();
                return false;
            }
        };
        let slot = msg.a as usize;
        if let Some(prev) = self.slot.replace(slot) {
            self.send(Msg::new(MSG_RELEASE, prev as u32, 0, 0));
        }
        let h = unsafe { &*self.shm.slot(slot) };
        self.frame_diff = h.frame_diff;
        self.aggregated_diff = h.aggregated_diff;
        self.frame_width = h.width as usize;
        self.frame_height = h.height as usize;
        self.used_width = h.used_width as usize;
        self.used_height = h.used_height as usize;
        let audio_len = (h.audio_len as usize).min(MAX_AUDIO);
        let audio = unsafe { std::slice::from_raw_parts(self.shm.audio(slot), audio_len) };
        self.audio_sum = audio.iter().map(|a| (*a as i32).abs()).sum();
        self.audio.extend_from_slice(audio);
        self.aspect_ratio = h.aspect_ratio;
        self.sample_rate = h.sample_rate;
        self.fps = h.fps;
        true
    }
    fn get_info(&self) -> Option<String> {
        self.info.clone()
    }

    fn focus(&mut self, focus: ViewFocus) {
        let a = match focus {
            ViewFocus::Invisible => 0,
            ViewFocus::Visible => 1,
            ViewFocus::Focus => 2,
        };
        self.send(Msg::new(MSG_FOCUS, a, 0, 0));
    }

    fn send_keys(&mut self, keys: &[(u32, u32)]) {
        self.send(Msg::new(MSG_SEND_KEYS, keys.len() as u32, 0, 0));
        for &(frame, code) in keys {
            self.send(Msg::new(MSG_KEY, frame, code, 0));
        }
    }

    fn is_silent(&self) -> bool {
        self.audio_sum.abs() < 1000
    }

    fn screen_changed(&self) -> bool {
        self.frame_diff > 0.0 || self.aggregated_diff > SCREEN_ACTIVE
    }

    fn get_number_of_disks(&mut self) -> u32 {
        self.disk_count
    }
    fn reset(&mut self) {
        self.send(Msg::new(MSG_RESET, 0, 0, 0));
    }
    fn set_disk(&mut self, no: u32) {
        self.send(Msg::new(MSG_SET_DISK, no, 0, 0));
    }
    fn press_key(&mut self, code: u32, down: bool, mods: u16) {
        self.send(Msg::new(MSG_PRESS_KEY, code, down as u32, mods as u32));
    }
    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.send(Msg {
            tag: MSG_MOUSE_MOTION,
            x: dx,
            y: dy,
            ..Default::default()
        });
    }
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.send(Msg::new(
            MSG_MOUSE_BUTTONS,
            left as u32,
            right as u32,
            middle as u32,
        ));
    }
    fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        self.send(Msg::new(MSG_JOYPAD, port, id, down as u32));
    }
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        let frame: &[u32] = match self.slot {
            Some(slot) => unsafe {
                let len = (self.frame_width * self.frame_height).min(MAX_PIXELS);
                std::slice::from_raw_parts(self.shm.pixels(slot), len)
            },
            None => &[],
        };
        f(self.frame_width, self.frame_height, frame);
    }
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.audio);
        self.audio.clear();
    }
    fn get_frame_size(&self) -> (usize, usize) {
        (self.frame_width, self.frame_height)
    }
    fn get_used_frame_size(&self) -> (usize, usize) {
        (self.used_width, self.used_height)
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

    fn skip_frames(&mut self, frames: u32) {
        set_state_bit(self.shm.atomics().1, STATE_SKIPPING, frames > 0);
        self.send(Msg::new(MSG_SKIP, frames, 0, 0));
    }
    fn state(&self) -> u64 {
        self.shm.atomics().1.load(Ordering::Relaxed)
    }
    fn frames_stepped(&self) -> u64 {
        self.shm.atomics().0.load(Ordering::Relaxed)
    }
}

impl Drop for RetroCoreProcess {
    fn drop(&mut self) {
        let _ = self.chan.send(Msg::new(MSG_UNLOAD, 0, 0, 0));
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    error!("Core did not shut down in {SHUTDOWN_TIMEOUT:?}, killing it");
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
                _ => return,
            }
        }
    }
}

/// If this process was started as a core worker, run it and return its exit code.
pub fn process_worker_main() -> Option<i32> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().map(|a| a.as_os_str()) != Some(OsStr::new(WORKER_ARG)) {
        return None;
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            "demarc=debug,warn"
        } else {
            "error"
        })
    });
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(filter)
        .compact()
        .try_init();

    let fd = |i: usize| -> Option<RawFd> { args.get(i)?.to_str()?.parse().ok() };
    let (Some(sock), Some(shm)) = (fd(1), fd(2)) else {
        error!("{WORKER_ARG} needs a socket and a shared memory fd");
        return Some(2);
    };
    let stream = unsafe { UnixStream::from_raw_fd(sock) };
    let file = unsafe { File::from_raw_fd(shm) };
    let worker = std::thread::Builder::new()
        .name("retro-emu".into())
        .stack_size(WORKER_STACK_SIZE)
        .spawn(move || run_worker(stream, file));
    match worker.map(|w| w.join()) {
        Ok(Ok(Ok(()))) => Some(0),
        Ok(Ok(Err(e))) => {
            error!("Core worker failed: {e:#}");
            Some(1)
        }
        _ => Some(1),
    }
}

fn run_worker(mut stream: UnixStream, file: File) -> Result<()> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let mut blob = vec![0u8; u32::from_ne_bytes(len) as usize];
    stream.read_exact(&mut blob)?;
    let mut fields = blob.split(|b| *b == 0);
    let mut next = || fields.next().context("truncated setup");
    let core_path = PathBuf::from(OsStr::from_bytes(next()?));
    let system_dir = PathBuf::from(OsStr::from_bytes(next()?));
    let game = Some(next()?)
        .filter(|g| !g.is_empty())
        .map(|g| PathBuf::from(OsStr::from_bytes(g)));
    let speed_test = next()? == b"1";
    let slots: usize = std::str::from_utf8(next()?)?.parse()?;
    let mut meta = HashMap::new();
    while let (Some(key), Some(val)) = (fields.next(), fields.next()) {
        meta.insert(
            String::from_utf8_lossy(key).into_owned(),
            String::from_utf8_lossy(val).into_owned(),
        );
    }

    let shm = Shm::map(&file, slots)?;
    drop(file);
    let mut chan = Channel::new(stream);
    let h = shm.header();

    let mut core = match RetroCoreDirect::new(&core_path, &system_dir, game.as_deref(), meta) {
        Ok(core) => core,
        Err(e) => {
            let e = e.to_string();
            let len = e.len().min(ERROR_LEN);
            unsafe {
                std::ptr::copy_nonoverlapping(e.as_ptr(), (&raw mut (*h).error).cast::<u8>(), len);
                (*h).error_len = len as u64;
            }
            chan.send(Msg::new(MSG_SETUP_ERR, 0, 0, 0))?;
            return Ok(());
        }
    };
    let (width, height) = core.get_frame_size();
    let (used_width, used_height) = core.get_used_frame_size();
    unsafe {
        (*h).fps = core.fps();
        (*h).width = width as u64;
        (*h).height = height as u64;
        (*h).used_width = used_width as u64;
        (*h).used_height = used_height as u64;
        (*h).disks = core.get_number_of_disks() as u64;
    }
    chan.send(Msg::new(MSG_SETUP_OK, 0, 0, 0))?;
    worker_loop(&mut core, &mut chan, &shm, speed_test);
    Ok(())
}

fn worker_loop(core: &mut RetroCoreDirect, chan: &mut Channel, shm: &Shm, speed_test: bool) {
    let (frames, state) = shm.atomics();
    let mut free: Vec<usize> = (0..shm.slots).rev().collect();
    let mut target = free.pop();
    // The slot holding the newest frame, which a duped frame is copied from.
    let mut last: Option<usize> = None;
    let mut key_queue: Vec<(u64, u32, bool)> = Vec::new();
    // Copy of the last frame handed over, to measure motion against.
    let mut last_frame: Vec<u32> = Vec::new();
    let mut aggregated_diff = 0.0f32;
    loop {
        let frame = frames.load(Ordering::Relaxed);

        // Drain commands, waiting for one while there is nothing to do.
        loop {
            let msg = match chan.recv(target.is_none() || !core.visible) {
                Ok(Some(msg)) => msg,
                Ok(None) => break,
                Err(_) => return,
            };
            match msg.tag {
                MSG_RELEASE => {
                    let slot = msg.a as usize;
                    if slot >= shm.slots {
                        continue;
                    }
                    if target.is_none() {
                        target = Some(slot);
                    } else {
                        free.push(slot);
                    }
                }
                MSG_SEND_KEYS => {
                    let mut time_code_list = Vec::with_capacity(msg.a as usize);
                    for _ in 0..msg.a {
                        match chan.recv_blocking() {
                            Ok(key) if key.tag == MSG_KEY => time_code_list.push((key.a, key.b)),
                            Ok(_) => {}
                            Err(_) => return,
                        }
                    }
                    let cmd = RetroCmd::SendKeys { time_code_list };
                    apply_cmd(core, cmd, &mut key_queue, frame, state);
                }
                _ => {
                    if let Some(cmd) = msg.to_cmd()
                        && apply_cmd(core, cmd, &mut key_queue, frame, state)
                    {
                        return;
                    }
                }
            }
        }

        if !key_queue.is_empty() {
            key_queue.retain(|&(at, code, down)| {
                if at > frame {
                    return true;
                }
                core.press_key(code, down, 0);
                false
            });
        }

        let Some(slot) = target else { continue };
        if !core.visible {
            continue;
        }
        core.frame_target = Some(FrameTarget {
            ptr: shm.pixels(slot),
            len: MAX_PIXELS,
        });
        core.run();
        let written = core.frame_target.take().is_none();
        if written {
            last = Some(slot);
        }
        frames.fetch_add(1, Ordering::Relaxed);
        if core.skip_frames > 0 {
            core.skip_frames -= 1;
            if core.skip_frames == 0 {
                set_state_bit(state, STATE_SKIPPING, false);
            }
            core.with_audio(|_| {});
            continue;
        }

        let (width, height) = core.get_frame_size();
        let len = (width * height).min(MAX_PIXELS);
        if !written
            && let Some(prev) = last
            && prev != slot
        {
            unsafe { std::ptr::copy_nonoverlapping(shm.pixels(prev), shm.pixels(slot), len) };
        }
        let pixels = unsafe { std::slice::from_raw_parts(shm.pixels(slot), len) };
        let frame_diff;
        (frame_diff, aggregated_diff) = get_frame_diff(pixels, &last_frame, aggregated_diff);
        last_frame.clear();
        last_frame.extend_from_slice(pixels);

        let mut audio_len = 0;
        core.with_audio(|s| {
            audio_len = s.len().min(MAX_AUDIO);
            unsafe { std::ptr::copy_nonoverlapping(s.as_ptr(), shm.audio(slot), audio_len) };
        });

        let (used_width, used_height) = core.get_used_frame_size();
        unsafe {
            shm.slot(slot).write(SlotHeader {
                width: width as u64,
                height: height as u64,
                used_width: used_width as u64,
                used_height: used_height as u64,
                frame_diff,
                aggregated_diff,
                sample_rate: core.sample_rate(),
                fps: core.fps(),
                aspect_ratio: core.aspect_ratio(),
                audio_len: audio_len as u32,
            })
        };
        last = Some(slot);

        if speed_test && free.is_empty() {
            // Benchmark: never wait on the consumer, render the next frame over this one.
            continue;
        }
        if chan.send(Msg::new(MSG_FRAME, slot as u32, 0, 0)).is_err() {
            return;
        }
        target = free.pop();
    }
}

const _: () = {
    fn _assert_send_sync<T: Send + Sync>() {}
    fn _check() {
        _assert_send_sync::<RetroCoreProcess>();
    }
};
