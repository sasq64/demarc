use crate::libloader;
use crate::m3u::M3u;
use crate::newsys::playstation::PSXSystem;
use crate::newsys::utils::{copy_dir_all, has_extension, read_header};
use crate::retro_emu::{Backend, RetroCoreThreaded};
use crate::system_dir;
use crate::workfile::WorkFile;
use amiga::AmigaSystem;
use anyhow::{Result, bail};
use c64::C64System;
use gameboy::GameboySystem;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use utils::{is_archive, unpack_into};

mod utils;

mod amiga;
mod c64;
mod gameboy;
mod playstation;

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
            let header = read_header(file.path(), header_size)?;
            let ext = file
                .path()
                .extension()
                .unwrap_or_default()
                .to_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            call(file.path(), &ext, &header);
        }
    }
    Ok(())
}

pub trait System {
    // NOTE: Is the useful?
    fn extensions(&self) -> &'static [&'static str] {
        &[]
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
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        self.extensions().contains(&ext.as_str())
    }

    fn get_first_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            println!("{path:?}");
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
fn get_systems() -> Vec<Box<dyn System>> {
    vec![
        Box::new(C64System {}),
        Box::new(GameboySystem {}),
        Box::new(AmigaSystem {}),
        Box::new(PSXSystem {}),
    ]
}

pub struct LoadResult {
    pub backend: Box<dyn Backend + Send + Sync>,
    pub name: String,
    pub work_file: WorkFile,
}

pub fn load_file(path: &Path, tags: &HashMap<String, String>) -> Result<LoadResult> {
    println!("LOAD_FILE: {path:?}");
    let mut wf = WorkFile::new(path);
    wf.tags = tags.clone();
    if path.is_file() {
        if is_archive(path)? {
            let tags = wf.tags;
            wf = WorkFile::new_dir()?;
            wf.tags = tags;
            println!("UNPACK {path:?} to {wf:?}");
            unpack_into(path, &wf).unwrap();
        } else if has_extension(path, "m3u") {
            let m3u = M3u::from_file(path)?;
            wf.path = path.parent().unwrap().to_owned();
            for (key, value) in m3u.tags {
                wf.tags.insert(key, value);
            }
        }
    }
    println!("CHECK");
    for sys in get_systems() {
        if sys.load(&mut wf).unwrap() {
            println!("Loading {:?}", &wf.path);
            if let Some(dir) = &wf.temp_dir {
                for entry in fs::read_dir(dir)? {
                    let path = entry?.path();
                    println!("  {path:?}");
                }
            };
            println!("TAGS: {:?}", &wf.tags);

            if let Some(td) = wf.temp_dir.as_ref() {
                copy_dir_all(td.path(), Path::new("last"))?;
            }

            return Ok(LoadResult {
                backend: sys.create(&wf)?,
                work_file: wf,
                name: sys.name().into(),
            });
        }
    }
    bail!("NO")
}

#[cfg(test)]
mod tests {

    use tracing_subscriber::{EnvFilter, fmt};

    use super::*;

    pub fn frame_bytes(pixels: &[u32]) -> &[u8] {
        unsafe {
            std::slice::from_raw_parts(pixels.as_ptr() as *const u8, std::mem::size_of_val(pixels))
        }
    }
    pub fn save_png(backend: &dyn Backend, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        backend.with_frame(&mut |width, height, pixels| {
            let expected = width * height;
            println!("{width} {height} {}", pixels.len());
            if width == 0 || height == 0 || pixels.len() < expected {
                return;
            }
            let bytes = frame_bytes(&pixels[..expected]).to_vec();
            let buf = image::RgbaImage::from_raw(width as u32, height as u32, bytes).unwrap();
            buf.save(path).unwrap();
        });
        Ok(())
    }

    fn init_tracing() {
        let _ = fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    fn test_load(path: &Path, name: &str) -> LoadResult {
        let mut result = load_file(path, &HashMap::new()).unwrap();
        println!("{:?}", result.work_file.tags);
        assert_eq!(result.name, name);
        result.backend.run();
        result
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
        let res = test_load(&testdata.join("o2-intro"), "Amiga");
        assert!(res.work_file.tags.get("puae_model") == Some(&"A500".to_string()));
        let res = test_load(&testdata.join("nexus7"), "Amiga");
        assert!(res.work_file.tags.get("puae_use_whdload") == Some(&"enabled".to_string()));
        assert!(res.work_file.tags.get("puae_model") == Some(&"A1200".to_string()));
    }
    #[test]
    fn test_psx() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("psx");
        test_load(&testdata.join("paradox").join("pdx-051.psx"), "PSX");
        test_load(&testdata.join("monophobia"), "PSX");
    }
}
