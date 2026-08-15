use crate::m3u::M3u;
use crate::newsys::amstrad::AmstradSystem;
use crate::newsys::atari_2600::Atari2600System;
use crate::newsys::atari_st::AtariStSystem;
use crate::newsys::atari_xl::AtariXlSystem;
use crate::newsys::gba::GBASystem;
use crate::newsys::images::ImageSystem;
use crate::newsys::megadrive::MegadriveSystem;
use crate::newsys::music::MusicSystem;
use crate::newsys::playstation::PSXSystem;
use crate::newsys::sinclair::SinclairSystem;
use crate::newsys::snes::SNESSystem;
use crate::newsys::tic80::Tic80System;
use crate::newsys::utils::{copy_dir_all, has_extension, read_at};
use crate::retro_emu::{Backend, RetroCoreThreaded};
use crate::system_dir;
use crate::workfile::WorkFile;
use crate::{Args, libloader};
use amiga::AmigaSystem;
use anyhow::{Result, bail};
use c64::C64System;
use gameboy::GameboySystem;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::info;
use utils::{is_archive, unpack_into};

mod utils;

mod amiga;
mod amstrad;
mod atari_2600;
mod atari_st;
mod atari_xl;
mod c64;
mod gameboy;
mod gba;
mod images;
mod megadrive;
mod music;
mod playstation;
mod sinclair;
mod snes;
mod tic80;

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
/// uncertain, prefer more common PSX. Tags can predecide if necessary
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
/// TAGS
///
/// Frontend merges argument tags with m3u tags first, according to "some" logic
///
/// System adds default tags that has not been set, and adds tags depending on content.
///
///
pub fn walk_dir(
    dir: &Path,
    header_size: usize,
    mut call: impl FnMut(&Path, &str, &[u8]) -> Result<()>,
) -> Result<()> {
    for f in walkdir::WalkDir::new(dir).into_iter() {
        let file = f?;
        if file.path().is_file() {
            // Short files are still worth handing to the callback, so take
            // whatever is there rather than demanding a full header.
            let header = read_at(file.path(), 0, header_size)?;
            let ext = file
                .path()
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            call(file.path(), &ext, &header)?;
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

    fn default_tags(&self) -> HashMap<&str, &str> {
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
        println!("LOAD: {file:?}");
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
        println!("PATH {path:?}");
        let core = libloader::get_libretro(self.core_name()).unwrap();
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            path.tags.clone(),
            false,
        )?))
    }
}

#[derive(Default)]
pub struct NewSys {
    systems: Vec<Box<dyn System>>,
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
            Box::new(ImageSystem {}),
            Box::new(AmstradSystem {}),
            Box::new(SinclairSystem {}),
            Box::new(Atari2600System {}),
            // Last: `musix` recognises a lot of files, and anything a real
            // system can run should go to that system instead.
            Box::new(MusicSystem {}),
        ]
    }
    pub fn new(args: &Args) -> Self {
        NewSys {
            systems: Self::get_systems(args),
        }
    }

    pub fn load_file(&self, path: &Path, tags: &HashMap<String, String>) -> Result<LoadResult<'_>> {
        println!("LOAD_FILE: {path:?}");
        let mut wf = WorkFile::new(path);
        wf.tags = tags.clone();
        if path.is_file() {
            if is_archive(path)? {
                let tags = wf.tags;
                wf = WorkFile::new_dir()?;
                wf.tags = tags;
                println!("UNPACK {path:?} to {wf:?}");
                unpack_into(path, &wf)?;
                walk_dir(&wf, 4, |f, _, _| {
                    if is_archive(f)? {
                        info!("Double packed");
                        unpack_into(f, &wf)?;
                    }
                    Ok(())
                })?;
            } else if has_extension(path, "m3u") {
                let m3u = M3u::from_file(path)?;
                wf.path = path.parent().unwrap().to_owned();
                for (key, value) in m3u.tags {
                    wf.tags.insert(key, value);
                }
            }
        }
        println!("CHECK");
        for sys in &self.systems {
            if sys.load(&mut wf).unwrap() {
                println!("Loading {:?}", &wf.path);
                if let Some(dir) = &wf.temp_dir {
                    for entry in fs::read_dir(dir)? {
                        let path = entry?.path();
                        println!("  {path:?}");
                    }
                };

                for (key, val) in sys.default_tags() {
                    if !wf.has_tag(key) {
                        wf.set_tag(key, val);
                    }
                }

                println!("TAGS: {:?}", &wf.tags);

                if let Some(td) = wf.temp_dir.as_ref() {
                    copy_dir_all(td.path(), Path::new("last"))?;
                }
                wf.set_tag("system", sys.name());

                return Ok(LoadResult {
                    backend: sys.create(&wf)?,
                    work_file: wf,
                    system: &sys,
                });
            }
        }
        bail!("NO")
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
        println!("{:?}", result.work_file.tags);
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
        test_load(&testdata.join("Skaaneland.zip"), "C64");
    }

    #[test]
    fn test_amiga() {
        init_tracing();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("amiga");
        test_load(&testdata.join("rebels.adf"), "Amiga");
        test_load(&testdata.join("o2-intro"), "Amiga");
        let work_file = test_load(&testdata.join("o2-intro").join("o2intro"), "Amiga");

        assert!(work_file.tags.get("puae_use_whdload") == Some(&"enabled".to_string()));
        assert!(work_file.tags.get("puae_model") == Some(&"A1200".to_string()));
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

    #[test]
    fn test_psx() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("psx");
        test_load(&testdata.join("paradox").join("pdx-051.psx"), "PSX");
        test_load(&testdata.join("monophobia"), "PSX");
    }
}
