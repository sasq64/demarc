use anyhow::Result;
use std::{collections::HashMap, fs};
use tracing::warn;

use super::utils::build_m3u;

use crate::{
    Args, cbmconvert,
    newsys::{utils::sort_disks, walk_dir},
    workfile::WorkFile,
};

use super::System;

const CORE_NAME_VICE_64SC: &str = "vice_x64sc";
// const CORE_NAME_VICE_64: &str = "vice_x64";
// const CORE_NAME_VICE_DTV: &str = "vice_x64dtv";
// const CORE_NAME_VICE_128: &str = "vice_x128";
// const CORE_NAME_VICE_C16: &str = "vice_xplus4";
// const CORE_NAME_VICE_VIC20: &str = "vice_xvic";

pub struct C64System {
    fast_load: bool,
}

impl C64System {
    pub fn new(args: &Args) -> Self {
        Self {
            fast_load: args.fast_load,
        }
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
        let conversions: HashMap<_, _> = [("t64", "-t"), ("lnx", "-l"), ("p00", "-p")].into();
        let mut need_conv = false;
        walk_dir(file, 4, |_, ext, _| {
            if conversions.contains_key(ext) {
                need_conv = true;
            }
            Ok(())
        })?;
        if need_conv {
            file.make_temp()?;
            // NOTE: If incoming was single file, we now switch to the parent dir
            file.path = file.temp_dir.as_ref().unwrap().path().to_owned();
        }
        println!("{file:?} {:?}", file.path);
        walk_dir(file, 4, |path, ext, _header| {
            println!("{path:?} {ext:?}");
            if let Some(flag) = conversions.get(ext) {
                println!("Converting {path:?}");
                let parent = path.parent().unwrap();
                let _guard = cbmconvert::CwdGuard::enter(parent);
                let code = cbmconvert::run([flag, "-N", path.to_string_lossy().as_ref()]);
                println!("Done");
                if code != 0 {
                    warn!("cbmconvert failed on {path:?} (exit code {code})");
                } else {
                    if fs::remove_file(path).is_err() {
                        warn!("Could not remove {path:?}");
                    }
                }
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
            if self.fast_load {
                file.set_meta("vice_cartridge", "rr38ppal-auto.crt");
                file.set_meta("vice_autostart", "disabled");
            }
            sort_disks(&mut images);
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
