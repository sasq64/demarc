//! What the systems want to know about how the user asked for the release to
//! run — the slice of demarc's command line that reaches machine detection.
//!
//! A plain struct rather than the `Args` it is built from: `Args` is the CLI,
//! and this crate has no business knowing what the CLI looks like.

use std::path::PathBuf;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum CbmSystem {
    /// Default Commodore C64
    #[default]
    C64,
    /// Commodore 128
    C128,
    /// C64 DTV Stick
    Dtv,
    /// C16/Plus4
    C16,
    /// VIC 20
    VIC20,
}

#[derive(Clone, Debug, Default)]
pub struct SysOpts {
    /// Amiga: force AGA (A1200 with 8MB Fast RAM).
    pub aga: bool,
    /// Commodore variant to emulate.
    pub cbm_variant: CbmSystem,
    /// `-x key=value` core options, applied over everything a system sets.
    pub extra_options: Vec<String>,
    /// Amiga: force high specs (68030 + FPU).
    pub fast: bool,
    /// C64: always load through the Retro Replay. Amiga: no disk rotation.
    pub fast_load: bool,
    /// Several emulators at once, which is what makes a system economise.
    pub grid: Option<(u32, u32)>,
    /// Max queued frames.
    pub latency: u32,
    /// Luau script for the music visualizer.
    pub lua: Option<PathBuf>,
    /// Run cores in a separate process instead of a thread.
    pub proc: bool,
    /// C64: add the 16MB RAM expansion unit.
    pub reu: bool,
    /// Amiga/C64/Amstrad: no disk loading sound.
    pub silent_drive: bool,
    /// Amiga: unpack a single-disk AmigaDOS demo out of its ADF and boot it as
    /// a hard drive.
    pub unadf: bool,
    /// Amiga/Atari ST: extra memory.
    pub xmem: bool,
}
