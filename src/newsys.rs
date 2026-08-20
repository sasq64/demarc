use crate::m3u::M3u;
use crate::newsys::amstrad::AmstradSystem;
use crate::newsys::atari_2600::Atari2600System;
use crate::newsys::atari_st::AtariStSystem;
use crate::newsys::atari_xl::AtariXlSystem;
use crate::newsys::gba::GBASystem;
use crate::newsys::images::ImageSystem;
use crate::newsys::megadrive::MegadriveSystem;
use crate::newsys::music::MusicSystem;
use crate::newsys::neo_geo::NeoGeoSystem;
pub use crate::newsys::neo_geo::holds_boot_list;
use crate::newsys::playstation::PSXSystem;
use crate::newsys::sinclair::SinclairSystem;
use crate::newsys::snes::SNESSystem;
use crate::newsys::tic80::Tic80System;
use crate::newsys::utils::{has_extension, read_at, sort_disks};
use crate::retro_emu::{Backend, RetroCoreThreaded};
use crate::system_dir;
use crate::workfile::WorkFile;
use crate::{Args, libloader};
use amiga::AmigaSystem;
use anyhow::{Context, Result, bail};
use c64::C64System;
use gameboy::GameboySystem;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, trace, warn};
use utils::{is_archive, unpack_into};

pub(crate) mod utils;

mod amiga;
mod amstrad;
mod atari_2600;
mod atari_st;
mod atari_xl;
mod c64;
mod disc;
mod gameboy;
mod gba;
mod images;
mod megadrive;
mod music;
mod neo_geo;
mod playstation;
mod sinclair;
mod snes;
mod tic80;

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
            let m3u = M3u::build(&images)?;
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

pub fn get_ext(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// A System is responsible for indentifying, converting, configuring and loading releases for
/// a particular (or series of) computer or console.
///
/// Loading procedure:
///
/// Frontend first downloads, unpacks and does any non-system specific conversions of a realese.
/// The result will be:
/// - A folder/file combination where either (but not both) may be None, and folder must be a
/// parent of the file.
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
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                if let Some(found) = self.get_first_file(&path)? {
                    return Ok(Some(found));
                }
                continue;
            } else if self.can_load(&path) {
                return Ok(Some(path.to_owned()));
            }
        }
        Ok(None)
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
    pub system: &'a Box<dyn System>,
    pub work_file: WorkFile,
}

impl NewSys {
    fn get_systems(args: &Args) -> Vec<Box<dyn System>> {
        vec![
            Box::new(Tic80System {}),
            Box::new(AmigaSystem::new(&args)),
            Box::new(AtariStSystem::new()),
            Box::new(AtariXlSystem::new(&args)),
            Box::new(C64System::new(&args)),
            Box::new(GameboySystem {}),
            Box::new(GBASystem::new(args)),
            Box::new(MegadriveSystem::new(args)),
            Box::new(SNESSystem::new(args)),
            Box::new(PSXSystem {}),
            Box::new(AmstradSystem {}),
            Box::new(SinclairSystem {}),
            Box::new(Atari2600System {}),
            Box::new(NeoGeoSystem {}),
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

    pub fn load_file(&self, path: &Path, meta: &HashMap<String, String>) -> Result<LoadResult<'_>> {
        debug!("Trying to load: {path:?}");
        let mut wf = WorkFile::new_with_meta(path, meta.clone());
        for (key, val) in &self.meta {
            wf.set_meta(key, val);
        }
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
                debug!("System {} can load {:?}", sys.name(), path);
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
                    system: &sys,
                });
            }
        }
        let dir_list = if wf.path.is_dir() {
            fs::read_dir(wf.path)?
                .into_iter()
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
mod tests {

    use clap::Parser;
    use tracing_subscriber::{EnvFilter, fmt};

    use super::*;

    fn init_tracing() {
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    fn test_load(path: &Path, name: &str) -> WorkFile {
        let args = Args::parse_from(["demarc"]);
        let s = NewSys::new(&args);

        let mut result = s.load_file(path, &HashMap::new()).unwrap();
        println!("{:?}", result.work_file.get_all_meta());
        assert_eq!(result.system.name(), name);
        result.backend.run();
        result.work_file
    }

    #[test]
    fn test_c64() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("c64");

        test_load(&testdata.join("quantum.prg"), "C64");
        test_load(&testdata.join("DEMO060A.rar"), "C64");
        test_load(&testdata.join("Maniacs of Noise Logo.t64.gz"), "C64");
        test_load(&testdata.join("cd"), "C64");
        assert!(!testdata.join("cd").join("demo.m3u").exists());
        test_load(&testdata.join("cd/The_Violators-CD_s1.d64"), "C64");
        test_load(&testdata.join("Skaaneland.zip"), "C64");
    }

    #[test]
    fn test_amiga() {
        init_tracing();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("amiga");
        test_load(&testdata.join("desert"), "Amiga");
        assert!(!testdata.join("desert").join("demo.m3u").exists());
        test_load(&testdata.join("desert").join("disk1.adf"), "Amiga");
        test_load(&testdata.join("desert.zip"), "Amiga");
        test_load(&testdata.join("rebels.adf"), "Amiga");
        test_load(&testdata.join("o2-intro"), "Amiga");

        // A plain executable is booted from a generated startup-sequence on a
        // stock A500, not through WHDLoad.
        let work_file = test_load(&testdata.join("o2-intro").join("o2intro"), "Amiga");
        assert!(work_file.get_meta("puae_use_whdload", "") == "disabled");
        assert!(work_file.get_meta("puae_model", "") == "A500");

        // A WHDLoad install (a `.slave` next to the data) turns WHDLoad on and
        // needs an A1200.
        let work_file = test_load(&testdata.join("nexus7"), "Amiga");
        assert!(work_file.get_meta("puae_use_whdload", "") == "enabled");
        assert!(work_file.get_meta("puae_model", "") == "A1200");
    }
    /// A bare music file has no system of its own, so it falls through every
    /// other system to [`MusicSystem`] — both on its own and as the only
    /// playable thing in a directory.
    #[test]
    fn test_music() {
        let dir = std::env::temp_dir().join("newsys_music_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let song = dir.join("tune.mod");
        crate::music_emu::write_test_mod(&song);

        test_load(&song, "Music");
        test_load(&dir, "Music");
    }

    /// DEGAS pictures reach [`ImageSystem`] both by extension and, since they
    /// are as often named after the release as `.pi1`, by content. A screenshot
    /// next to one doesn't win over it.
    #[test]
    fn test_degas_images() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("degas");
        test_load(&testdata.join("FUSE.PI1"), "Images");
        test_load(&testdata.join("BOLEK3.PC1"), "Images");

        let dir = std::env::temp_dir().join("newsys_degas_test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Named so the walk reaches the screenshot first, and with the
        // extension stripped so only the sniff can find the picture.
        fs::copy(testdata.join("FUSE.PI1"), dir.join("zz-picture")).unwrap();
        fs::write(dir.join("aa-shot.png"), b"not really a png").unwrap();

        let work_file = test_load(&dir, "Images");
        assert!(
            work_file.path.ends_with("zz-picture"),
            "picked {:?} over the DEGAS picture",
            work_file.path
        );
    }

    #[test]
    fn test_psx() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("psx");
        test_load(&testdata.join("paradox").join("pdx-051.psx"), "PSX");
        test_load(&testdata.join("monophobia"), "PSX");
        // A bare data track with no cue beside it, named `.bin` like any other
        // dump, is recognised from the disc's own contents.
        test_load(&testdata.join("thisispsx"), "PSX");
    }
}
