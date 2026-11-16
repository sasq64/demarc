//! Finding what to boot in a C64 release, and the VICE options it needs.

use anyhow::{Context, Result};
use std::{collections::HashMap, fs, path::Path};
use tracing::warn;

use retro_core::backend::InputEvent;
use retro_core::libretro::{RETROK_F1, RETROK_RETURN};

use crate::cbmconvert;
use crate::release::{collect_disk_images, walk_dir};
use crate::workfile::WorkFile;

/// Keys that take the fast-load cartridge's menu into the disk, by frame.
pub const FAST_LOAD_EVENTS: [(u32, InputEvent); 2] = [
    (50, InputEvent::Key(RETROK_F1)),
    (55, InputEvent::Key(RETROK_RETURN)),
];

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

/// The core has no option for a third SID, but reads `<save dir>/vicerc`.
fn three_sids(file: &mut WorkFile) -> Result<()> {
    file.make_temp()?;
    let Some(temp) = file.temp_dir() else {
        warn!("No temp dir for the vicerc; playing with one SID");
        return Ok(());
    };
    let saves = temp.join(".vice-saves");
    fs::create_dir_all(&saves)?;
    fs::write(
        saves.join("vicerc"),
        "[C64SC]\nSidStereo=2\nSid2AddressStart=54304\nSid3AddressStart=54336\n",
    )
    .context("Could not write vicerc")?;
    file.set_meta("save_dir", saves.to_string_lossy());
    Ok(())
}

/// Pick the file VICE should start out of `file` and set the options the
/// release needs. `false` when there is nothing for a C64 in it.
pub fn prepare(file: &mut WorkFile, reu: bool) -> Result<bool> {
    let mut images = vec![];
    let mut prgs = vec![];

    if reu || file.has_tag("reu") || file.get_meta_or("category", "").contains("REU") {
        file.set_meta("vice_ram_expansion_unit", "16384kB");
    }

    if file.has_tag("3sid") {
        three_sids(file)?;
    } else if file.has_tag("2sid") {
        file.set_meta("vice_sid_extra", "0xd420");
    }
    if file.has_tag("6581") {
        file.set_meta("vice_sid_model", "6581");
    } else {
        file.set_meta("vice_sid_model", "8580");
    }

    let conversions: HashMap<_, _> = [("t64", "-t"), ("lnx", "-l"), ("p00", "-p")].into();
    let mut need_conv = false;
    let mut has_reu = false;
    walk_dir(file, 4, |_, ext, _| {
        if conversions.contains_key(ext) {
            need_conv = true;
        }
        has_reu |= ext == "reu";
        Ok(())
    })?;
    // The REU image gets renamed, so it must not be the user's own copy
    if need_conv || has_reu {
        // NOTE:
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

    let mut reus = vec![];
    let mut tapes = vec![];
    walk_dir(file, 12, |path, ext, header| {
        if ext == "reu" {
            reus.push(path.to_owned());
        } else if ["d64", "d81"].contains(&ext) {
            images.push(path.to_owned());
        } else if is_c64_prg(path, ext, header) || ext == "crt" {
            prgs.push(path.to_owned());
        } else if ext == "tap" && header.starts_with(b"C64-TAPE-RAW") {
            tapes.push(path.to_owned());
        }
        Ok(())
    })?;

    if !images.is_empty() {
        if !file.is_disabled("c64_fast_load") {
            file.set_meta("vice_cartridge", "rr38ppal-auto.crt");
            file.set_meta("vice_autostart", "disabled");
        }
        collect_disk_images(file, &mut images)?;
    } else if !prgs.is_empty() {
        file.path = prgs[0].clone();
    } else if !tapes.is_empty() {
        tapes.sort();
        file.path = tapes[0].clone();
    } else {
        return Ok(false);
    }

    // VICE picks up `<name>.reu` next to the loaded file as the REU image
    if let Some(reu) = reus.first() {
        let target = file.path.with_extension("reu");
        if *reu != target {
            fs::rename(reu, &target).with_context(|| format!("Renaming {reu:?}"))?;
        }
    }
    Ok(true)
}

#[cfg(test)]
#[path = "tests/c64_tests.rs"]
mod tests;
