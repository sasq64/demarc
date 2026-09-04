use anyhow::{Context, Result, bail};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tracing::{debug, info, trace, warn};

use crate::backend::Backend;
use crate::emu_file::{Override, Patch};
use crate::m3u::M3u;
use crate::retro_emu::RetroCoreThreaded;
use crate::system_dir;
use crate::workfile::WorkFile;
use crate::{Args, libloader};

use crate::utils::{get_ext, has_extension, read_at, sort_disks};
use crate::utils::{is_archive, unpack_into};

use amiga::AmigaSystem;
use amstrad::AmstradSystem;
use atari_2600::Atari2600System;
use atari_st::AtariStSystem;
use atari_xl::AtariXlSystem;
use c64::C64System;
use dos::DosSystem;
use gameboy::GameboySystem;
use gba::GBASystem;
use images::ImageSystem;
use megadrive::MegadriveSystem;
use music::MusicSystem;
use neo_geo::NeoGeoSystem;
use pico8::Pico8System;
use playstation::PSXSystem;
use plus4::Plus4System;
use sinclair::SinclairSystem;
use snes::SNESSystem;
use std::collections::HashMap;
use tic80::Tic80System;
use windows::WindowsSystem;

mod adf;
mod amiga;
mod amstrad;
mod atari_2600;
mod atari_st;
mod atari_xl;
mod c64;
mod disc;
mod dos;
mod gameboy;
mod gba;
mod images;
mod megadrive;
mod music;
mod neo_geo;
mod pico8;
mod playstation;
mod plus4;
mod sinclair;
mod snes;
mod tic80;
mod windows;

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

/// Walk `dir`, calling `call` with the path, lowercased extension and the
/// first `header_size` bytes of every file big enough to have them.
pub fn walk_dir(
    dir: &Path,
    header_size: usize,
    mut call: impl FnMut(&Path, &str, &[u8]) -> Result<()>,
) -> Result<()> {
    walk_dir_find(dir, header_size, |path, ext, header| {
        trace!("WALK: {path:?}");
        call(path, ext, header)?;
        Ok(None::<()>)
    })?;
    Ok(())
}

/// [`walk_dir`] for searches: the first `Some` the callback returns ends the
/// walk and comes back as the result.
pub fn walk_dir_find<T>(
    dir: &Path,
    header_size: usize,
    mut call: impl FnMut(&Path, &str, &[u8]) -> Result<Option<T>>,
) -> Result<Option<T>> {
    for f in walkdir::WalkDir::new(dir).into_iter() {
        let file = f?;
        if file
            .file_name()
            .to_str()
            .unwrap_or_default()
            .starts_with(".")
        {
            continue;
        }
        if file.path().is_file() {
            let header = read_at(file.path(), 0, header_size)?;
            if header.len() == header_size {
                let ext = file
                    .path()
                    .extension()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if let Some(res) = call(file.path(), &ext, &header)? {
                    return Ok(Some(res));
                }
            }
        }
    }
    Ok(None)
}

pub fn collect_disk_images(file: &mut WorkFile, images: &mut [PathBuf]) -> Result<()> {
    if !images.is_empty() {
        if images.len() > 1 {
            info!("IMAGES {images:?}");
            sort_disks(images);
            info!("IMAGES {images:?}");
            // images can be in a non-writeable or temp dir
            let m3u = M3u::build(images)?;
            file.make_temp()?;
            let demo_m3u = file
                .temp_dir()
                .context("Should now be in tempdir")?
                .join("demo.m3u");
            info!("m3u: {demo_m3u:?}");
            m3u.relocate(&demo_m3u)?;
            file.path = demo_m3u;
        } else {
            file.path = images[0].clone();
        }
    }
    Ok(())
}

/// Unpack a downloaded release, producing the [`WorkFile`] that
/// [`NewSys::load_prepared`] takes over from.
///
/// Split out from the rest because it is the one expensive step that touches no
/// shared state at all: it reads `path` and writes into a temp dir of its own,
/// so the frontend runs it on the I/O pool while the release currently on
/// screen keeps playing (see `Emulator::load_async`). On the main thread it
/// cost a visible stutter right where it is least wanted — a double-packed
/// release is unpacked twice, and that landed on the very frame a cross-fade
/// was starting. What is left for the main thread (detection, conversion,
/// building the backend) either needs the system table or is the core itself.
///
/// Archives are unpacked one level deep and then once more, because scene
/// releases are routinely packed inside another archive. An m3u is not
/// unpacked at all; its tags become meta and the directory it names is what
/// gets loaded.
pub fn unpack_release(path: &Path, meta: &HashMap<String, String>) -> Result<WorkFile> {
    debug!("Trying to load: {path:?}");
    let mut wf = WorkFile::new_with_meta(path, meta.clone());
    if path.is_file() {
        if is_archive(path)? {
            wf = WorkFile::new_dir_with_meta(meta.clone())?;
            debug!("Unpacking {path:?} to {wf:?}");
            unpack_into(path, &wf)?;
            walk_dir(&wf, 4, |f, _, _| {
                if is_archive(f)? {
                    debug!("File was double packed");
                    unpack_into(f, &wf)?;
                }
                Ok(())
            })?;
        } else if has_extension(path, "m3u") {
            // TODO: We should not collect m3us
            let m3u = M3u::from_file(path)?;
            wf.path = path.parent().unwrap_or(path).to_owned();
            for (key, value) in m3u.tags {
                wf.set_meta(&key, value);
            }
        }
    }
    Ok(wf)
}

/// Meta key holding the release directory a `boot_file` was picked out of, set
/// by [`apply_override`] when it narrows the work file's path to one program.
/// A system that copies a release into a drive of its own reads it to know that
/// what it is holding is one file out of a release rather than a loose program.
pub const RELEASE_DIR: &str = "release_dir";

/// Apply what `overrides.toml` says about this release, once it is unpacked and
/// before any system looks at it — see [`crate::overrides`].
///
/// The parts are independent and any of them may be absent: `fast` and meta go
/// on the [`WorkFile`], patches are written into the release, and `boot_file`
/// points the path at the one program to start so that the systems' own file
/// picking is skipped.
///
/// `fast` goes on first, because it is a whole Amiga configuration written as
/// one word (see [`amiga::apply_fast`]) and an entry that also names an option
/// of its own means that one to stand.
fn apply_override(file: &mut WorkFile, over: &Override) -> Result<()> {
    if over.fast {
        debug!("Override asks for the fast Amiga configuration");
        amiga::apply_fast(file);
    }
    for (key, val) in &over.meta {
        debug!("Override sets {key}={val}");
        file.set_meta(key, *val);
    }
    if !over.patches.is_empty() {
        apply_patches(file, &over.patches)?;
    }
    if let Some(boot) = over.boot_file {
        match find_named(&release_dir(file), boot)? {
            Some(path) => {
                info!("Override starts {path:?}");
                // A system tells "one loose program the user pointed at" from
                // "a whole release" by whether the work file is a file or a
                // directory, and copies the data files along only in the
                // second case (`copy_all` in `newsys::amiga`). Narrowing the
                // path to the named program throws that away, so leave behind
                // the release it came out of.
                let dir = release_dir(file).to_string_lossy().into_owned();
                file.set_meta(RELEASE_DIR, dir);
                file.path = path;
            }
            // The archive it named is not the one that was downloaded, most
            // likely. Falling back to the systems' own pick still runs
            // something, which beats refusing to load the release at all.
            None => warn!("Override names {boot:?}, which is not in {:?}", file.path),
        }
    }
    Ok(())
}

/// The directory holding the release, whether the work file points at the
/// directory itself or at one file inside it.
fn release_dir(file: &WorkFile) -> PathBuf {
    if file.path.is_dir() {
        file.path.clone()
    } else {
        file.path.parent().unwrap_or(Path::new(".")).to_owned()
    }
}

/// Write an override's patches into the release.
///
/// A patch is nearly always a config file the release was packed without: a
/// DOS demo asks its own `.CFG` where the sound card is, and without one it
/// either runs silent or refuses to start. So a target that isn't there yet is
/// created rather than skipped, in the directory the release was unpacked to.
///
/// The release is copied somewhere writable first, since it may just as well be
/// a plain directory on disk as a temp dir full of unpacked files — and the one
/// thing a patch must not do is edit the user's own copy of a release.
fn apply_patches(file: &mut WorkFile, patches: &[Patch]) -> Result<()> {
    file.make_temp()?;
    let dir = release_dir(file);
    for patch in patches {
        let data = patch.bytes()?;
        let target = match find_named(&dir, patch.target)? {
            Some(path) => path,
            None => dir.join(patch.target),
        };
        write_patch(&target, patch.offset, &data)
            .with_context(|| format!("Could not patch {target:?}"))?;
        info!(
            "Patched {target:?} with {} bytes{}",
            data.len(),
            if patch.info.is_empty() {
                String::new()
            } else {
                format!(" ({})", patch.info)
            }
        );
    }
    Ok(())
}

/// Write `data` into `target`: replacing it entirely when there is no offset,
/// or overwriting the bytes at `offset` when there is. A file too short to
/// reach the offset is extended with zeros, the way DOS itself would.
fn write_patch(target: &Path, offset: Option<usize>, data: &[u8]) -> Result<()> {
    let Some(offset) = offset else {
        return Ok(fs::write(target, data)?);
    };
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(target)?;
    let offset = offset as u64;
    if out.metadata()?.len() < offset {
        out.set_len(offset)?;
    }
    out.seek(SeekFrom::Start(offset))?;
    out.write_all(data)?;
    Ok(())
}

/// The file called `name` anywhere inside `dir`, ignoring case — the names in a
/// DOS release come back from an archive in every case there is, and an override
/// is written from what the demo's own documentation calls the file.
fn find_named(dir: &Path, name: &str) -> Result<Option<PathBuf>> {
    walk_dir_find(dir, 0, |path, _ext, _header| {
        let found = path
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f.eq_ignore_ascii_case(name));
        Ok(found.then(|| path.to_owned()))
    })
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
    // NOTE: Is the useful?
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn handles_ext(&self, path: &Path) -> bool {
        self.extensions().contains(&get_ext(path).as_str())
    }

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

    fn get_first_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        walk_dir_find(dir, 0, |file, _ext, _header| {
            if self.can_load(file) {
                return Ok(Some(file.to_owned()));
            };
            Ok(None)
        })
    }

    // Try to load a program with this system. WorkFile may change. On successful
    // result, WorkFile can be used with create() to actually start emulation.
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if file.is_dir() {
            if let Some(path) = self.get_first_file(file)? {
                file.path = path;
                return Ok(true);
            }
        } else if self.can_load(file) {
            return Ok(true);
        }
        Ok(false)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let core = libloader::get_libretro(self.core_name()).context("Could not load core")?;
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            path.get_all_meta(),
            false,
        )?))
    }
}

#[derive(Default)]
pub struct NewSys {
    systems: Vec<Box<dyn System>>,
    meta: HashMap<String, String>,
}
pub struct LoadResult<'a> {
    pub backend: Box<dyn Backend + Send + Sync>,
    pub system: &'a dyn System,
    pub work_file: WorkFile,
}

impl NewSys {
    fn get_systems(args: &Args) -> Vec<Box<dyn System>> {
        vec![
            Box::new(Tic80System {}),
            Box::new(Pico8System {}),
            Box::new(AmigaSystem::new(args)),
            Box::new(AtariStSystem::new()),
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
            Box::new(AmstradSystem {}),
            Box::new(SinclairSystem {}),
            Box::new(Atari2600System {}),
            Box::new(NeoGeoSystem {}),
            Box::new(DosSystem {}),
            Box::new(WindowsSystem {}),
            Box::new(MusicSystem::new(args)),
            Box::new(ImageSystem {}),
        ]
    }
    pub fn new(args: &Args) -> Self {
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
        NewSys {
            systems: Self::get_systems(args),
            meta,
        }
    }

    /// Load a release, with `over` carrying whatever `overrides.toml` had to
    /// say about it (see [`crate::overrides`]) and `None` when it had nothing.
    ///
    /// The two halves are also callable separately, and the frontend does that:
    /// [`unpack_release`] runs on the I/O pool while the previous release is
    /// still on screen, and only [`load_prepared`](Self::load_prepared) has to
    /// happen on the main thread. So outside the tests nothing takes this
    /// route any more; it stays as the one place the whole pipeline is written
    /// out in order.
    #[allow(dead_code)]
    pub fn load_file(
        &self,
        path: &Path,
        meta: &HashMap<String, String>,
        over: Option<&Override>,
    ) -> Result<LoadResult<'_>> {
        self.load_prepared(unpack_release(path, meta)?, over)
    }

    /// Finish loading an already-[unpacked](unpack_release) release: apply the
    /// override, fill in meta, find the system that claims it and build its
    /// backend.
    pub fn load_prepared(
        &self,
        mut wf: WorkFile,
        over: Option<&Override>,
    ) -> Result<LoadResult<'_>> {
        // Now that the release is unpacked and its own meta is in place: an
        // override may write files into it, name the one to start and set meta
        // of its own, which beats what the release says about itself.
        if let Some(over) = over {
            apply_override(&mut wf, over)?;
        }

        // Last, so that `-x` on the command line beats every other source.
        for (key, val) in &self.meta {
            debug!("Adding {key}={val}");
            wf.set_meta(key, val);
        }

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
        for sys in &self.systems {
            trace!("Trying to load with {}", sys.name());
            if sys.load(&mut wf)? {
                debug!("System {} can load {:?}", sys.name(), wf.path);
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
                return Ok(LoadResult {
                    backend: sys.create(&wf)?,
                    work_file: wf,
                    system: sys.as_ref(),
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
