//! How a core's dmabuf frames reach the frontend's GPU without a CPU copy. The
//! frontend supplies the [`FrameImporter`]; this crate only moves fds, slot
//! numbers and the opaque [`GpuFrame`] it hands back. See
//! `docs/GAMESCOPE_ZEROCOPY.md`.

use std::os::fd::BorrowedFd;
use std::sync::OnceLock;

use crate::backend::GpuFrame;

/// One buffer of a core's ring.
pub struct DmabufBuffer<'a> {
    pub fd: BorrowedFd<'a>,
    pub stride: u32,
    pub offset: u32,
    pub size: u64,
}

/// A core's ring of XRGB8888 linear buffers.
pub struct DmabufSet<'a> {
    pub width: u32,
    pub height: u32,
    pub buffers: Vec<DmabufBuffer<'a>>,
}

pub trait FrameImporter: Send {
    /// Import the ring, replacing any earlier one. The fds are borrowed.
    fn set_buffers(&mut self, bufs: &DmabufSet) -> bool;
    /// Start copying `slot` into a frame of the frontend's own. `done` runs,
    /// on any thread, once the copy no longer reads the slot.
    fn import(&mut self, slot: usize, done: Box<dyn FnOnce() + Send>) -> Option<GpuFrame>;
}

type Factory = Box<dyn Fn() -> Option<Box<dyn FrameImporter>> + Send + Sync>;

static FACTORY: OnceLock<Factory> = OnceLock::new();

/// Lets cores loaded from now on hand their frames to importers `factory` makes.
pub fn set_frame_importer(
    factory: impl Fn() -> Option<Box<dyn FrameImporter>> + Send + Sync + 'static,
) {
    let _ = FACTORY.set(Box::new(factory));
}

pub(crate) fn new_importer() -> Option<Box<dyn FrameImporter>> {
    if std::env::var_os("DEMARC_NO_DMABUF").is_some_and(|v| v != "0") {
        return None;
    }
    FACTORY.get()?()
}
