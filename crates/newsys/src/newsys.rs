use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tracing::{debug, info, trace, warn};

use crate::backend::Backend;
use crate::release_override::{Override, Patch};
use crate::retro_emu;
use crate::system_dir;
use crate::workfile::WorkFile;
use crate::{SysOpts, cache, fetch, libloader};

use crate::utils::get_ext;

use amiga::AmigaSystem;
use amstrad::AmstradSystem;
use atari_2600::Atari2600System;
use atari_st::AtariStSystem;
use atari_xl::AtariXlSystem;
use c64::C64System;
use dos::DosSystem;
use ffmpeg::FfmpegSystem;
use gameboy::GameboySystem;
use gba::GBASystem;
use images::ImageSystem;
use lynx::LynxSystem;
use megadrive::MegadriveSystem;
use microw8::MicroW8System;
use music::MusicSystem;
use neo_geo::NeoGeoSystem;
use pico8::Pico8System;
use playstation::PSXSystem;
use plus4::Plus4System;
use psp::PspSystem;
use sinclair::SinclairSystem;
use snes::SNESSystem;
use spectrum_next::SpectrumNextSystem;
use std::collections::{HashMap, HashSet};
use tic80::Tic80System;
use web::WebSystem;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use windows::WindowsSystem;

mod adf;
mod amiga;
mod amstrad;
mod atari_2600;
mod atari_st;
mod atari_xl;
mod c64;
mod disc;
mod dms;
mod dos;
mod ffmpeg;
mod gameboy;
mod gba;
mod images;
mod lynx;
mod megadrive;
mod microw8;
mod music;
pub use music::SongName;
pub use release_io::release::{
    ARCHIVE_NAME, SFX, collect_disk_images, unpack_release, walk_dir, walk_dir_find,
};
mod neo_geo;
mod pico8;
mod playstation;
mod plus4;
mod psp;
mod sinclair;
mod snes;
mod spectrum_next;
mod tic80;
mod web;
// Windows runs them itself and Linux runs them under wine; everywhere else a
// `.exe` with a `PE` image in it is something nothing here can run — and
// claiming it would take the release away from the picture and music systems
// that can at least show what it shipped beside the program.
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod windows;

/// Meta key saying whether the screen demarc is drawing to is a widescreen one.
/// Set by the frontend from the window, since nothing further down knows the
/// shape of the screen — see `crate::wine::default_dialog_res`, which is what
/// asks.
pub const META_WIDESCREEN: &str = "widescreen";

/// Meta key holding the refresh rate, in whole Hz, of the screen demarc is
/// drawing to. Set by the frontend from the monitor, for the same reason as
/// [`META_WIDESCREEN`] — see `crate::newsys::windows`, which turns it into the
/// rate a gamescope session is paced at.
pub const META_REFRESH: &str = "screen_refresh";

/// Trim the caches of built and rewritten discs back under their budgets.
///
/// Intended to run once at startup, alongside [`crate::fetch::prune_cache`] and
/// for the same reason: nothing is holding a path into any of them yet, so this
/// run's own work can't be evicted out from under it.
///
/// Each cache carries its own budget, since what an entry costs differs by two
/// orders of magnitude between them — see the constant beside each one.
pub fn prune_caches() {
    disc::prune_cache();
    neo_geo::prune_cache();
    playstation::prune_cache();
}

/// The caches [`prune_caches`] trims, by name, and what each holds in bytes.
pub fn temp_cache_sizes() -> [(&'static str, u64); 3] {
    [
        ("cdda", disc::cache_size()),
        ("neocd", neo_geo::cache_size()),
        ("psxexe", playstation::cache_size()),
    ]
}

/// [`unpack_release`] for a release with `cache_writeback` set: the first time,
/// the unpacked and patched release replaces its download in the cache, and
/// from then on it runs from there.
pub fn unpack_release_writeback(
    path: &Path,
    meta: &HashMap<String, String>,
    over: &Override,
) -> Result<WorkFile> {
    let mut wf = unpack_release(path, meta)?;
    if cache::is_writeback(path) {
        info!("Running {path:?} from the cache");
        if let Some(stem) = path.file_stem() {
            wf.set_meta(ARCHIVE_NAME, stem.to_string_lossy());
        }
        return Ok(WorkFile::new_in_place(path, wf.get_all_meta()));
    }
    if !wf.is_temporary() || !fetch::is_download(path) {
        warn!("Can't write {path:?} back to the cache");
        return Ok(wf);
    }
    apply_patches(&mut wf, &over.patches)?;
    cache::write_back(path, &wf.path)?;
    info!("Wrote {path:?} back to the cache");
    Ok(WorkFile::new_in_place(path, wf.get_all_meta()))
}

pub use release_io::release_override::{BOOT_COMMAND, RELEASE_DIR, apply_override_meta};

/// [`release_io::apply_override`], after the one part of an override that
/// belongs to a system: `fast` goes on first, because it is a whole Amiga
/// configuration written as one word (see [`amiga::apply_fast`]) and an entry
/// that also names an option of its own means that one to stand.
fn apply_override(file: &mut WorkFile, over: &Override) -> Result<()> {
    if over.fast {
        debug!("Override asks for the fast Amiga configuration");
        amiga::apply_fast(file);
    }
    release_io::apply_override(file, over, system_dir())
}

fn apply_patches(file: &mut WorkFile, patches: &[Patch]) -> Result<()> {
    release_io::apply_patches(file, patches, system_dir())
}

/// A System is responsible for indentifying, converting, configuring and loading releases for
/// a particular (or series of) computer or console.
///
/// Loading procedure:
///
/// Frontend first downloads, unpacks and does any non-system specific conversions of a realese.
/// The result will be:
/// - A folder/file combination where either (but not both) may be None, and folder must be a
///   parent of the file.
/// - Existing m3u files should be handled by frontend and not passed on to loading.
///
/// Result will be passed to load_file() of each system in order. First that succeeds will be used.
/// Use ordering for priority in case of uncertain detection.
///
/// FILE EXAMPLES:
/// demo.t64
///
/// Loaded and converted by C64, extension unique
///
/// demo.m3u
/// Can not be passed to system. Frontend will extract meta data and pass on directory instead.
///
/// demo.cue
/// Systems must parse cue and look at corresponding bin/iso to detect PSX or Neo Geo. If
/// uncertain, prefer more common PSX. Meta can predecide if necessary
///
/// DIRECTORY (MOST COMMON):
/// Possible outcomes:
/// - Single valid file. Sort files in prio order and pick first
///   (ie PRG over IFF over PNG, CUE over ISO)
/// - Disk images. Collect, sort, and write m3u (Amiga multi URL download)
/// - HDD. Amiga/Atari. detected by EXE file. Optional startup-sequence.
///
/// What frontend can help with: Smart dir walk
///
/// META
///
/// Frontend merges argument meta with m3u tags first, according to "some" logic
///
/// System adds default meta that has not been set, and adds meta depending on content.
///
/// `Send + Sync` so that `Box<dyn System>` — and with it [`NewSys`] — can be
/// held by a Bevy resource. All implementors are plain data, so this costs
/// nothing, and it keeps this module free of any bevy dependency.
pub trait System: Send + Sync {
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn handles_ext(&self, path: &Path) -> bool {
        self.extensions().contains(&get_ext(path).as_str())
    }

    // Systems defaulting to gamepad control should return true here
    fn is_console(&self) -> bool {
        false
    }

    // The libretro core to use, if any
    fn core_name(&self) -> &'static str {
        ""
    }

    // Name of the system
    fn name(&self) -> &'static str;

    fn default_meta(&self) -> HashMap<&str, &str> {
        HashMap::new()
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path)
    }

    // Try to load a program with this system. WorkFile may change. On successful
    // result, WorkFile can be used with create() to actually start emulation.
    // On non-succesful, WorkFile is assumed to be unchanged.
    //
    // Default implementation returns first file it "can load".
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if file.is_dir() {
            if let Some(path) = get_first_file(self, file)? {
                file.path = path;
                return Ok(true);
            }
        } else if self.can_load(file) {
            return Ok(true);
        }
        Ok(false)
    }

    // Create a Backend from a WorkFile. WorkFile must have been succefully passed to load()
    // earlier.
    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let core = libloader::get_libretro(self.core_name()).context("Could not load core")?;
        retro_emu::create_core(&core, system_dir(), Some(path), path.get_all_meta(), false)
    }
}

fn get_first_file(sys: &(impl System + ?Sized), dir: &Path) -> Result<Option<PathBuf>> {
    walk_dir_find(dir, 0, |file, _ext, _header| {
        if sys.can_load(file) {
            return Ok(Some(file.to_owned()));
        };
        Ok(None)
    })
}

#[derive(Default)]
pub struct NewSys {
    systems: Vec<Box<dyn System>>,
    /// Behind a lock because the whole of [`NewSys::load_prepared`] runs on a
    /// job thread while the settings dialog may write here (see
    /// [`NewSys::set_meta`]). Read once per load, so the lock is never hot.
    meta: Mutex<HashMap<String, String>>,
}

/// What a finished [`NewSys::load_prepared`] hands back. Owned rather than
/// borrowing the [`System`] that claimed the release, so it can be carried back
/// from the job thread that built it.
pub struct LoadResult {
    pub backend: Box<dyn Backend + Send + Sync>,
    pub work_file: WorkFile,
    pub system_name: &'static str,
    pub is_console: bool,
}

impl NewSys {
    fn get_systems(args: &SysOpts) -> Vec<Box<dyn System>> {
        vec![
            // Lynx releases often bundle a Windows emulator; the magic check is strict.
            Box::new(LynxSystem {}),
            Box::new(DosSystem {}),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            Box::new(WindowsSystem {}),
            Box::new(Tic80System {}),
            Box::new(MicroW8System {}),
            Box::new(Pico8System {}),
            Box::new(AmigaSystem::new(args)),
            Box::new(AtariStSystem::new(args)),
            Box::new(AtariXlSystem::new(args)),
            // Before the C64, which would otherwise claim the same disks and
            // programs; it stands aside unless --cbm-variant asked for it.
            Box::new(Plus4System::new(args)),
            Box::new(C64System::new(args)),
            Box::new(GameboySystem {}),
            Box::new(GBASystem::new(args)),
            Box::new(MegadriveSystem::new(args)),
            Box::new(SNESSystem::new(args)),
            Box::new(PSXSystem {}),
            Box::new(PspSystem {}),
            Box::new(AmstradSystem {}),
            Box::new(SpectrumNextSystem {}),
            Box::new(SinclairSystem {}),
            Box::new(Atari2600System {}),
            Box::new(NeoGeoSystem {}),
            Box::new(FfmpegSystem {}),
            Box::new(MusicSystem::new(args)),
            Box::new(ImageSystem {}),
            Box::new(WebSystem {}),
        ]
    }
    /// Every extension some system claims, lowercase.
    pub fn supported_extensions() -> &'static HashSet<&'static str> {
        static EXTENSIONS: OnceLock<HashSet<&'static str>> = OnceLock::new();
        EXTENSIONS.get_or_init(|| {
            Self::get_systems(&SysOpts::default())
                .iter()
                .flat_map(|sys| sys.extensions().iter().copied())
                .collect()
        })
    }

    /// The files under `dir` some system could start on their own, sorted.
    pub fn loadable_files(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut files = release_io::c64::bootable_files(dir)?;
        walk_dir(dir, 0, |path, ext, _| {
            if Self::supported_extensions().contains(ext) && !files.iter().any(|f| f == path) {
                files.push(path.to_owned());
            }
            Ok(())
        })?;
        files.sort();
        Ok(files)
    }

    pub fn new(args: &SysOpts) -> Self {
        let mut meta = HashMap::<String, String>::new();
        for opt in &args.extra_options {
            if let Some((key, val)) = opt.split_once("=") {
                meta.insert(key.trim().into(), val.trim().into());
            }
        }
        if args.grid.is_some() {
            // TODO: Maybe insert "grid" and let core decide?
            meta.insert("psx_core".into(), "beetle".into());
        }
        meta.insert("latency".into(), args.latency.to_string());
        if args.proc {
            meta.insert("use_proc".into(), "1".into());
        }
        NewSys {
            systems: Self::get_systems(args),
            meta: Mutex::new(meta),
        }
    }

    /// Change one of the run-wide meta values set up by [`NewSys::new`] — how
    /// the settings dialog moves `latency`.
    ///
    /// Applied in [`load_prepared`](Self::load_prepared), so it takes hold on
    /// the next release loaded, not on the one playing: a backend reads its
    /// meta once, as it is built.
    pub fn set_meta(&self, key: &str, value: String) {
        self.meta_mut().insert(key.into(), value);
    }

    pub fn meta_mut(&self) -> MutexGuard<'_, HashMap<String, String>> {
        self.meta.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Has one of them been set already? What `-x` said is in here too, so this
    /// is how the frontend leaves a value someone typed alone.
    pub fn has_meta(&self, key: &str) -> bool {
        self.meta_mut().contains_key(key)
    }

    /// The systems to try, in order. A browser release often ships screenshots
    /// and music beside its page, which the systems ahead of Web would claim.
    fn systems_for(&self, wf: &WorkFile) -> Vec<&dyn System> {
        let web_first = wf
            .get_meta_or("platform", "")
            .split(';')
            .any(|p| p.trim().eq_ignore_ascii_case("browser"));
        let (mut order, rest): (Vec<&dyn System>, Vec<&dyn System>) = self
            .systems
            .iter()
            .map(|sys| sys.as_ref())
            .partition(|sys| web_first && sys.name() == "Web");
        order.extend(rest);
        order
    }

    /// Load a release, with `over` carrying whatever `overrides.toml` had to
    /// say about it (see [`crate::overrides`]) and `None` when it had nothing.
    ///
    /// The two halves are also callable separately, and the frontend does that:
    /// [`unpack_release`] runs on the I/O pool while the previous release is
    /// still on screen, and [`load_prepared`](Self::load_prepared) follows it
    /// on a job of its own. So outside the tests nothing takes this route any
    /// more; it stays as the one place the whole pipeline is written out in
    /// order.
    #[allow(dead_code)]
    pub fn load_file(
        &self,
        path: &Path,
        meta: &HashMap<String, String>,
        over: Option<&Override>,
    ) -> Result<LoadResult> {
        self.load_prepared(unpack_release(path, meta)?, over)
    }

    /// Last, so that `-x` on the command line beats every other source.
    fn apply_cmdline_meta(&self, wf: &mut WorkFile) {
        for (key, val) in self.meta_mut().iter() {
            debug!("Adding {key}={val}");
            wf.set_meta(key, val);
        }
    }

    /// Finish loading an already-[unpacked](unpack_release) release: apply the
    /// override, fill in meta, find the system that claims it and build its
    /// backend.
    ///
    /// Runs on a job thread (see
    /// [`Emulator::start_create`](crate::emulator::Emulator::start_create)), so
    /// everything a [`System`] reaches for here has to be shareable — which is
    /// why [`NewSys`] holds its meta behind a lock.
    pub fn load_prepared(&self, mut wf: WorkFile, over: Option<&Override>) -> Result<LoadResult> {
        // Crinkler exes are unpacked before an override patches them, so its
        // offsets are into the unpacked code; its meta can turn that off.
        if let Some(over) = over {
            apply_override_meta(&mut wf, over);
        }
        self.apply_cmdline_meta(&mut wf);
        crate::com_dropper::unpack_release(&mut wf)?;
        crate::crinkler::unpack_release(&mut wf)?;
        crate::boxedapp::unpack_release(&mut wf)?;

        // Now that the release is unpacked and its own meta is in place: an
        // override may write files into it, name the one to start and set meta
        // of its own, which beats what the release says about itself.
        if let Some(over) = over {
            apply_override(&mut wf, over)?;
        }

        self.apply_cmdline_meta(&mut wf);

        // Sort out which side of a disc release we were pointed at before any
        // system looks at it, since a directory holding a cue and its tracks is
        // handed to us as one file at a time.
        if wf.path.is_file() {
            let ext = get_ext(&wf.path);
            if ext == "cue" {
                // A sheet that can't find one of its files is unloadable — no
                // core will open it — so step over it to the directory it sits
                // in and let the systems find the image on their own.
                if !disc::cue_is_complete(&wf.path)
                    && let Some(dir) = wf.path.parent()
                {
                    warn!(
                        "Skipping {:?}: it references files that aren't there",
                        wf.path
                    );
                    wf.path = dir.to_owned();
                }
            } else if disc::TRACK_EXTENSIONS.contains(&ext.as_str())
                && let Some(cue) = disc::cue_for_track(&wf.path)
            {
                // The track on its own leaves the disc's CD audio behind, and
                // the sheet beside it doesn't.
                info!("Loading {:?} through {cue:?}", wf.path);
                wf.path = cue;
            }
        }
        for sys in self.systems_for(&wf) {
            trace!("Trying to load with {}", sys.name());
            if sys.load(&mut wf)? {
                debug!("System {} can load {:?}", sys.name(), wf.path);
                // Again, so they also beat what the system derived in `load`.
                if let Some(over) = over {
                    apply_override_meta(&mut wf, over);
                }
                self.apply_cmdline_meta(&mut wf);
                // Whichever system claimed the release, a cue's MP3 audio tracks
                // are unplayable to every core here — they read the compressed
                // bytes straight through as PCM — so the sheet is rewritten with
                // those decoded before the core opens it. A disc that needs
                // nothing comes back untouched.
                if get_ext(&wf.path) == "cue" {
                    match disc::prepare_disc(&wf.path) {
                        Ok(Some(prepared)) => wf.path = prepared,
                        Ok(None) => {}
                        // A sheet we can't rewrite is still worth handing over
                        // as it stands; the core may make more of it than we do.
                        Err(err) => warn!("Could not prepare {:?}: {err}", wf.path),
                    }
                }

                for (key, val) in sys.default_meta() {
                    if !wf.has_meta(key) {
                        wf.set_meta(key, val);
                    }
                }
                wf.set_meta("system", sys.name());

                debug!("Creating {:?} with meta {:?}", &wf.path, wf.get_all_meta());
                let mut backend = sys.create(&wf)?;
                if let Some(over) = over
                    && !over.events.is_empty()
                {
                    backend.send_events(&over.events);
                }
                return Ok(LoadResult {
                    backend,
                    work_file: wf,
                    system_name: sys.name(),
                    is_console: sys.is_console(),
                });
            }
        }
        let dir_list = if wf.path.is_dir() {
            fs::read_dir(wf.path)?
                .filter_map(Result::ok)
                .fold("DIR:\n".to_string(), |t, f| {
                    format!("{t}  {}\n", f.file_name().to_string_lossy())
                })
        } else {
            wf.path
                .file_name()
                .context("Path has no filename")?
                .to_string_lossy()
                .to_string()
        };
        bail!("No system recognized for: {dir_list}");
    }
}

#[cfg(test)]
#[path = "tests/newsys_tests.rs"]
mod tests;
