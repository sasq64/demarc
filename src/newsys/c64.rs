use anyhow::{Context, Result};
use std::{fs, path::Path};
use tracing::{info, warn};

use super::utils::{build_m3u, has_extension, unpack_into};

use crate::{cbmconvert, newsys::walk_dir, workfile::WorkFile};

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
    fn core_name(&self) -> &'static str {
        CORE_NAME_VICE_64SC
    }

    fn name(&self) -> &'static str {
        "C64"
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut prgs = vec![];
        println!("LOAD C64: {file:?}");
        walk_dir(file, 4, |path, ext, _header| {
            println!("{path:?} {ext:?}");
            if ext == "t64" {
                println!("Converting {path:?}");
                let parent = path.parent().unwrap();
                let _guard = cbmconvert::CwdGuard::enter(parent);
                let code = cbmconvert::run(["-t", "-N", path.to_string_lossy().as_ref()]);
                if code != 0 {
                    warn!("cbmconvert failed on {path:?} (exit code {code})");
                } else {
                    fs::remove_file(path).unwrap();
                }
            } else if ext == "gz" {
                unpack_into(path, &path.parent().context("Expect file to have parent")?)?;
                fs::remove_file(path)?;
            }
            Ok(())
        })?;

        walk_dir(file, 4, |path, ext, header| {
            println!("{path:?} {ext:?}");
            if ["d64", "d81"].contains(&ext) {
                images.push(path.to_owned());
            } else if ext == "prg" || (header[0] == 0x1 && header[1] == 0x8) {
                prgs.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            let m3u = build_m3u(&images, file)?;
            file.path = m3u;
        } else if !prgs.is_empty() {
            file.path = prgs[0].clone();
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
