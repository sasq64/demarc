use anyhow::{Context, Result};
use std::{fs, path::Path};
use tracing::{info, warn};

use super::utils::{build_m3u, get_disk_images, has_extension, unpack_into};

use crate::{cbmconvert, workfile::WorkFile};

use super::System;

const CORE_NAME_VICE_64SC: &str = "vice_x64sc";
// const CORE_NAME_VICE_64: &str = "vice_x64";
// const CORE_NAME_VICE_DTV: &str = "vice_x64dtv";
// const CORE_NAME_VICE_128: &str = "vice_x128";
// const CORE_NAME_VICE_C16: &str = "vice_xplus4";
// const CORE_NAME_VICE_VIC20: &str = "vice_xvic";

pub struct C64System {}

impl C64System {
    fn convert_files(path: &Path) -> Result<()> {
        if path.is_dir() {
            for entry in fs::read_dir(path)? {
                let path = entry?.path();
                Self::convert_files(&path)?;
            }
        } else if has_extension(&path, "t64") {
            info!("Converting {path:?}");
            let _guard = cbmconvert::CwdGuard::enter(path.parent().unwrap());
            let code = cbmconvert::run(["-t", "-N", path.to_string_lossy().as_ref()]);
            if code != 0 {
                warn!("cbmconvert failed on {path:?} (exit code {code})");
            } else {
                fs::remove_file(path)?;
            }
        } else if has_extension(&path, "gz") {
            unpack_into(path, &path.parent().context("Expect file to have parent")?)?;
            fs::remove_file(path)?;
        }
        Ok(())
    }
}

impl System for C64System {
    fn extensions(&self) -> &'static [&'static str] {
        &["d64", "prg", "d81", "t64"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_VICE_64SC
    }

    fn name(&self) -> &'static str {
        "C64"
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        println!("LOAD C64: {file:?}");
        if file.is_dir() || has_extension(file, "t64") {
            file.make_temp()?;
            Self::convert_files(file)?;
            let scanned = get_disk_images(file, &["d64", "d81"])?;
            println!("DIR {scanned:?}");
            if !scanned.is_empty() {
                let m3u = build_m3u(&scanned, file)?;
                file.path = m3u;
            } else {
                if let Some(path) = self.get_first_file(file)? {
                    file.path = path;
                    return Ok(true);
                }
                return Ok(false);
            }
        } else {
            if !self.can_load(file) {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
