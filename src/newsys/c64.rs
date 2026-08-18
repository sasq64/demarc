use anyhow::Result;
use std::{collections::HashMap, fs, path::Path};
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

/// A PRG is a 2-byte little endian load address followed by the data to place
/// there, so it can never reach past the top of the C64's 64K address space.
/// VICE rejects anything that does, and other systems use the same extension
/// (Neo Geo, for one), so check the range instead of trusting the name.
fn is_c64_prg(path: &Path, ext: &str, header: &[u8]) -> bool {
    let [lo, hi, ..] = header else { return false };
    if ext != "prg" && !(*lo == 0x01 && *hi == 0x08) {
        return false;
    }
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    // Load address plus at least one byte of data.
    let Some(data_size) = meta.len().checked_sub(2).filter(|n| *n > 0) else {
        return false;
    };
    let start_addr = u64::from(u16::from_le_bytes([*lo, *hi]));
    start_addr + data_size <= 0x1_0000
}

pub struct C64System {
    fast_load: bool,
    reu: bool,
}

impl C64System {
    pub fn new(args: &Args) -> Self {
        Self {
            fast_load: args.fast_load,
            reu: args.reu,
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

        if self.reu {
            file.set_meta("vice_ram_expansion_unit", "16384kB");
        }

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
        walk_dir(file, 4, |path, ext, _header| {
            if let Some(flag) = conversions.get(ext) {
                let parent = path.parent().unwrap();
                let _guard = cbmconvert::CwdGuard::enter(parent);
                let code = cbmconvert::run([flag, "-N", path.to_string_lossy().as_ref()]);
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
            if ["d64", "d81"].contains(&ext) {
                images.push(path.to_owned());
            } else if is_c64_prg(path, ext, header) {
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
