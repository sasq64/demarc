//! The core's half of [`crate::frame_import`]: answering
//! [`RETRO_ENVIRONMENT_DEMARC_DMABUF`], and holding each ring slot until its GPU
//! copy is done and a newer frame has replaced it, so a screenshot can still
//! read the newest one.

use std::ffi::{c_int, c_uint, c_void};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::sync::{Arc, Mutex};

use tracing::{info, warn};

use super::{CURRENT_EMU, RetroCoreDirect};
use crate::backend::GpuFrame;
use crate::frame_import::{DmabufBuffer, DmabufSet, FrameImporter, new_importer};
use crate::libretro::RETRO_ENVIRONMENT_PRIVATE;
use crate::pixels::{convert_xrgb8888, sample_runs};

/// demarc's own, answered only when there is an importer.
pub const RETRO_ENVIRONMENT_DEMARC_DMABUF: u32 = RETRO_ENVIRONMENT_PRIVATE | 2;

const DRM_FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

type SetBuffersFn = unsafe extern "C" fn(
    c_uint,
    c_uint,
    u32,
    c_uint,
    *const c_int,
    *const u32,
    *const u32,
    *const u64,
) -> bool;
type ReleaseFn = unsafe extern "C" fn(c_uint);

/// `struct demarc_dmabuf_iface` in the gamescope core's `core.cpp`.
#[repr(C)]
pub struct DmabufIface {
    set_buffers: Option<SetBuffersFn>,
    present: Option<unsafe extern "C" fn(c_uint) -> bool>,
    release: Option<ReleaseFn>,
}

const DMA_BUF_SYNC_READ: u64 = 1;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 4;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;

fn dma_sync(fd: BorrowedFd, flags: u64) {
    let flags = flags | DMA_BUF_SYNC_READ;
    loop {
        let ret = unsafe { libc::ioctl(fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC as _, &flags) };
        if ret == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

struct Mapping {
    ptr: *mut c_void,
    len: usize,
    fd: OwnedFd,
    stride: usize,
    offset: usize,
}

// Read-only shared memory, only read under the ring's lock.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr, self.len) };
    }
}

impl Mapping {
    fn new(fd: BorrowedFd, size: u64, stride: u32, offset: u32) -> Option<Self> {
        let fd = fd.try_clone_to_owned().ok()?;
        let len = size as usize;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return None;
        }
        Some(Self {
            ptr,
            len,
            fd,
            stride: stride as usize,
            offset: offset as usize,
        })
    }

    fn fits(&self, width: usize, height: usize) -> bool {
        height > 0
            && self.stride % 4 == 0
            && self.offset % 4 == 0
            && self.offset + (height - 1) * self.stride + width * 4 <= self.len
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.cast::<u8>(), self.len) }
    }

    /// The pixels [`sample_runs`] names, as raw XRGB.
    fn gather(&self, width: usize, height: usize, out: &mut Vec<u32>) {
        out.clear();
        dma_sync(self.fd.as_fd(), DMA_BUF_SYNC_START);
        let bytes = self.bytes();
        for run in sample_runs(width * height, width) {
            let mut i = run.start;
            while i < run.end {
                let (y, x) = (i / width, i % width);
                let n = (width - x).min(run.end - i);
                let row = &bytes[self.offset + y * self.stride + x * 4..][..n * 4];
                // The mapping is page aligned, and stride and offset are whole pixels.
                out.extend_from_slice(unsafe {
                    std::slice::from_raw_parts(row.as_ptr().cast::<u32>(), n)
                });
                i += n;
            }
        }
        dma_sync(self.fd.as_fd(), DMA_BUF_SYNC_END);
    }
}

#[derive(Default)]
struct RingState {
    width: usize,
    height: usize,
    maps: Vec<Mapping>,
    /// Holds on each slot: one for a GPU copy in flight, one for being newest.
    refs: Vec<u8>,
    newest: Option<usize>,
    /// The core's, so `None` once it is being unloaded.
    release: Option<ReleaseFn>,
}

impl RingState {
    fn unref(&mut self, slot: usize) {
        let Some(r) = self.refs.get_mut(slot) else {
            return;
        };
        *r = r.saturating_sub(1);
        if *r == 0
            && let Some(release) = self.release
        {
            unsafe { release(slot as c_uint) };
        }
    }
}

/// Shared by the core thread, the GPU's completion callbacks and
/// `with_frame` on the frontend's thread.
#[derive(Default)]
pub struct Ring(Mutex<RingState>);

impl Ring {
    fn lock(&self) -> std::sync::MutexGuard<'_, RingState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// No slot is handed back to the core after this.
    pub fn detach(&self) {
        self.lock().release = None;
    }

    /// The newest GPU frame's pixels, converted as `video_refresh` would.
    pub fn with_newest(&self, f: &mut dyn FnMut(usize, usize, &[u32])) -> bool {
        let state = self.lock();
        let Some(map) = state.newest.and_then(|s| state.maps.get(s)) else {
            return false;
        };
        let (width, height) = (state.width, state.height);
        let mut pixels = vec![0u32; width * height];
        dma_sync(map.fd.as_fd(), DMA_BUF_SYNC_START);
        convert_xrgb8888(
            &map.bytes()[map.offset..],
            &mut pixels,
            width,
            height,
            map.stride,
        );
        dma_sync(map.fd.as_fd(), DMA_BUF_SYNC_END);
        f(width, height, &pixels);
        true
    }
}

pub struct Dmabuf {
    importer: Box<dyn FrameImporter>,
    pub ring: Arc<Ring>,
    active: bool,
    /// Every frame through `video_refresh` instead.
    pub cpu_frames: bool,
    /// The frame the last `present` made, with its [`sample_runs`] pixels.
    pub frame: Option<(GpuFrame, Vec<u32>)>,
    warned: bool,
}

impl Dmabuf {
    pub fn new() -> Option<Self> {
        Some(Self {
            importer: new_importer()?,
            ring: Arc::default(),
            active: false,
            cpu_frames: false,
            frame: None,
            warned: false,
        })
    }

    pub fn answer(&mut self, iface: &mut DmabufIface) {
        iface.set_buffers = Some(set_buffers_cb);
        iface.present = Some(present_cb);
        self.ring.lock().release = iface.release;
    }

    fn set_buffers(&mut self, width: usize, height: usize, buffers: &[DmabufBuffer]) -> bool {
        self.active = false;
        let mut maps = Vec::new();
        for b in buffers {
            match Mapping::new(b.fd, b.size, b.stride, b.offset) {
                Some(map) if map.fits(width, height) => maps.push(map),
                _ => return false,
            }
        }
        let set = DmabufSet {
            width: width as u32,
            height: height as u32,
            buffers: maps
                .iter()
                .map(|m| DmabufBuffer {
                    fd: m.fd.as_fd(),
                    stride: m.stride as u32,
                    offset: m.offset as u32,
                    size: m.len as u64,
                })
                .collect(),
        };
        if !self.importer.set_buffers(&set) {
            return false;
        }
        let mut state = self.ring.lock();
        state.refs = vec![0; maps.len()];
        state.maps = maps;
        state.newest = None;
        (state.width, state.height) = (width, height);
        self.active = true;
        true
    }

    fn fall_back(&mut self, why: &str) -> Option<(usize, usize)> {
        if !self.warned {
            self.warned = true;
            warn!("dmabuf: not zero-copy, frames go through the CPU ({why})");
        }
        None
    }

    fn present(&mut self, slot: usize) -> Option<(usize, usize)> {
        if self.cpu_frames {
            return None;
        }
        if !self.active {
            return self.fall_back("ring not imported");
        }
        let mut samples = Vec::new();
        let size = {
            let mut state = self.ring.lock();
            if state.refs.get(slot) != Some(&0) {
                drop(state);
                return self.fall_back("ring slot still in use");
            }
            state.maps[slot].gather(state.width, state.height, &mut samples);
            state.refs[slot] = 2;
            (state.width, state.height)
        };
        let ring = Arc::clone(&self.ring);
        let frame = self
            .importer
            .import(slot, Box::new(move || ring.lock().unref(slot)));
        let mut state = self.ring.lock();
        let Some(frame) = frame else {
            // The core gives it back itself.
            state.refs[slot] = 0;
            drop(state);
            return self.fall_back("GPU import failed");
        };
        if let Some(old) = state.newest.replace(slot) {
            state.unref(old);
        }
        self.frame = Some((frame, samples));
        Some(size)
    }
}

unsafe extern "C" fn set_buffers_cb(
    width: c_uint,
    height: c_uint,
    format: u32,
    count: c_uint,
    fds: *const c_int,
    stride: *const u32,
    offset: *const u32,
    size: *const u64,
) -> bool {
    let emu = CURRENT_EMU.with(|p| p.get());
    let Some(dmabuf) = (unsafe { emu.as_mut() }).and_then(|e| e.dmabuf.as_mut()) else {
        return false;
    };
    if format != DRM_FORMAT_XRGB8888 || count == 0 {
        warn!("dmabuf: format {format:#x} is not XRGB8888");
        return false;
    }
    let n = count as usize;
    let (fds, stride, offset, size) = unsafe {
        (
            std::slice::from_raw_parts(fds, n),
            std::slice::from_raw_parts(stride, n),
            std::slice::from_raw_parts(offset, n),
            std::slice::from_raw_parts(size, n),
        )
    };
    let buffers: Vec<_> = (0..n)
        .map(|i| DmabufBuffer {
            fd: unsafe { BorrowedFd::borrow_raw(fds[i]) },
            stride: stride[i],
            offset: offset[i],
            size: size[i],
        })
        .collect();
    let ok = dmabuf.set_buffers(width as usize, height as usize, &buffers);
    if ok {
        info!("dmabuf: {width}x{height} ring of {n} imported");
    } else {
        warn!("dmabuf: {width}x{height} ring of {n} not imported, frames go through the CPU");
    }
    ok
}

unsafe extern "C" fn present_cb(slot: c_uint) -> bool {
    let emu = CURRENT_EMU.with(|p| p.get());
    let Some(emu) = (unsafe { emu.as_mut() }) else {
        return false;
    };
    let Some((width, height)) = emu.dmabuf.as_mut().and_then(|d| d.present(slot as usize)) else {
        return false;
    };
    emu.state.frame_width = width;
    emu.state.frame_height = height;
    true
}

impl RetroCoreDirect {
    /// The frame `present` made during the last `run`, if any.
    pub(crate) fn take_gpu_frame(&mut self) -> Option<(GpuFrame, Vec<u32>)> {
        self.dmabuf.as_mut()?.frame.take()
    }

    pub(crate) fn set_cpu_frames(&mut self, cpu: bool) {
        if let Some(d) = &mut self.dmabuf {
            d.cpu_frames = cpu;
        }
    }
}
