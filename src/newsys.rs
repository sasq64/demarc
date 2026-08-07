use crate::libloader;
use crate::retro_emu::{Backend, RetroCoreThreaded};
use crate::system_dir;
use crate::workfile::WorkFile;
use anyhow::{Result, bail};
use c64::C64System;
use gameboy::GameboySystem;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use utils::{is_archive, unpack_into};

mod utils;

mod c64;
mod gameboy;

pub trait System {
    fn extensions(&self) -> &'static [&'static str] {
        &[]
    }

    fn core_name(&self) -> &'static str {
        ""
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

    // Try to load a program with this system.
    fn load(&self, file: &mut WorkFile, _tags: &HashMap<String, String>) -> Result<bool> {
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

    fn create(
        &self,
        path: &WorkFile,
        tags: &HashMap<String, String>,
    ) -> Result<Box<dyn Backend + Send + Sync>> {
        println!("PATH {path:?}");
        let core = libloader::get_libretro(self.core_name()).unwrap();
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            tags.clone(),
            false,
        )?))
    }
}
fn get_systems() -> Vec<Box<dyn System>> {
    vec![Box::new(C64System {}), Box::new(GameboySystem {})]
}

pub fn load_file(path: &Path) -> Result<Box<dyn Backend + Send + Sync>> {
    println!("LOAD_FILE: {path:?}");
    let mut wf = WorkFile::new(path);
    if path.is_file() && is_archive(path)? {
        wf = WorkFile::new_dir()?;
        println!("UNPACK {path:?} to {wf:?}");
        unpack_into(path, &wf).unwrap();
    }
    println!("CHECK");
    for sys in get_systems() {
        if sys.load(&mut wf, &HashMap::new()).unwrap() {
            return sys.create(&wf, &HashMap::new());
        }
    }
    bail!("NO")
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_prg() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let demos = root.join("demos");
        let testdata = root.join("testdata").join("c64");
        let mut backend = load_file(&testdata.join("quantum.prg")).unwrap();
        backend.run();
        let mut backend = load_file(&testdata.join("cd")).unwrap();
        backend.run();
        let mut backend = load_file(&testdata.join("Skaaneland.zip")).unwrap();
        backend.run();
        let mut backend = load_file(&testdata.join("Maniacs of Noise Logo.t64.gz")).unwrap();
        backend.run();

        let mut backend = load_file(&demos.join("nightmode.gb")).unwrap();
        backend.run();
    }
}
