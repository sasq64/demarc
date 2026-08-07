use crate::libloader;
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

pub trait System {
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn core_name(&self) -> &'static str {
        ""
    }
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
    ]
}

pub fn load_file(path: &Path) -> Result<(String, Box<dyn Backend + Send + Sync>)> {
    println!("LOAD_FILE: {path:?}");
    let mut wf = WorkFile::new(path);
    if path.is_file() && is_archive(path)? {
        wf = WorkFile::new_dir()?;
        println!("UNPACK {path:?} to {wf:?}");
        unpack_into(path, &wf).unwrap();
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
            let res = (sys.name().to_string(), sys.create(&wf)?);
            wf.temp_dir.unwrap().keep();
            return Ok(res);
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

    fn test_load(path: &Path) {
        let (name, mut backend) = load_file(path).unwrap();
        backend.run();
    }

    #[test]
    fn test_prg() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let demos = root.join("demos");
        let testdata = root.join("testdata").join("c64");
        let (name, mut backend) = load_file(&testdata.join("quantum.prg")).unwrap();
        backend.run();

        let (name, mut backend) = load_file(&testdata.join("cd")).unwrap();
        backend.run();
        let (name, mut backend) = load_file(&testdata.join("Skaaneland.zip")).unwrap();
        backend.run();
        let (name, mut backend) = load_file(&testdata.join("DEMO060A.rar")).unwrap();
        backend.run();
        let (name, mut backend) =
            load_file(&testdata.join("Maniacs of Noise Logo.t64.gz")).unwrap();
        backend.run();

        let (name, mut backend) = load_file(&demos.join("nightmode.gb")).unwrap();
        assert_eq!("Gameboy", name);
        backend.run();
    }

    #[test]
    fn test_amiga() {
        init_tracing();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let testdata = root.join("testdata").join("amiga");
        let (name, mut backend) = load_file(&testdata.join("rebels.adf")).unwrap();
        assert_eq!("Amiga", name);
        backend.run();
        let (name, mut backend) = load_file(&testdata.join("o2-intro")).unwrap();
        assert_eq!("Amiga", name);
        backend.run();
        let (name, mut backend) = load_file(&testdata.join("nexus7")).unwrap();
        assert_eq!("Amiga", name);
        backend.run_frames(400);
        save_png(backend.as_mut(), &root.join("amiga.png")).unwrap();
    }
}
