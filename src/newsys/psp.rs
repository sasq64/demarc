use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::{utils::read_header, workfile::WorkFile};

use super::System;
use super::disc::DiscImage;
use super::walk_dir;

const CORE_NAME: &str = "ppsspp";

pub struct PspSystem {}

/// A PBP container, or a bare PSP ELF executable (ELF32 LE, MIPS, ET_EXEC) —
/// scene releases for the 1.5 kxploit ship the latter under the name
/// `EBOOT.PBP`. PRX modules (e_type 0xffa0) are not executables.
pub fn is_psp_exe(path: &Path) -> bool {
    read_header(path, 20).is_ok_and(|h| {
        h.starts_with(b"\0PBP") || (h.starts_with(b"\x7fELF\x01\x01") && h[16..20] == [2, 0, 8, 0])
    })
}

pub fn is_psp_disc(path: &Path) -> bool {
    if read_header(path, 4).is_ok_and(|h| h == b"CISO") {
        return true;
    }
    DiscImage::open(path).is_some_and(|mut disc| {
        disc.root_names()
            .iter()
            .any(|name| name == "UMD_DATA.BIN" || name == "PSP_GAME")
    })
}

/// Whether anything but `exe` itself sits in its directory: the data a
/// release loads relative to its executable.
fn has_data_beside(exe: &Path) -> bool {
    exe.parent()
        .and_then(|dir| dir.read_dir().ok())
        .is_some_and(|entries| entries.flatten().any(|e| e.path() != exe))
}

/// The memory stick root above `exe`, if it already sits in `PSP/GAME/<name>/`.
fn memstick_root(exe: &Path) -> Option<&Path> {
    let game = exe.parent()?.parent()?;
    let psp = game.parent()?;
    (game.file_name()? == "GAME" && psp.file_name()? == "PSP").then(|| psp.parent())?
}

/// Put the release holding `exe` on a memory stick in its temp dir, at
/// `PSP/GAME/<name>/`, and hand that to the core as its save dir, which PPSSPP
/// uses as `ms0:`. Anywhere else PPSSPP boots it as `host0:/<file>`, and
/// homebrew that takes its data directory from `argv[0]` (Suicide Barbie) then
/// looks for its files on psplink's `host1:`.
fn put_on_memstick(file: &mut WorkFile, exe: &Path) -> Result<()> {
    let base = if file.is_dir() {
        file.path.clone()
    } else {
        file.path.parent().context("no parent")?.to_owned()
    };
    let rel = exe.strip_prefix(&base)?.to_owned();
    file.make_temp()?;
    let root = file.temp_dir().context("no temp dir")?;
    let exe = root.join(rel);

    if let Some(stick) = memstick_root(&exe) {
        file.set_meta("save_dir", stick.to_string_lossy());
        file.path = exe;
        return Ok(());
    }
    let src = exe.parent().context("no parent")?;
    let name = if src == root {
        "DEMARC".into()
    } else {
        src.file_name().context("no name")?.to_owned()
    };
    let game = root.join("PSP").join("GAME").join(name);
    fs::create_dir_all(&game)?;
    if src == root {
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if entry.file_name() != "PSP" {
                fs::rename(entry.path(), game.join(entry.file_name()))?;
            }
        }
    } else {
        fs::remove_dir(&game)?;
        fs::rename(src, &game)?;
    }
    file.path = game.join(exe.file_name().context("no name")?);
    file.set_meta("save_dir", root.to_string_lossy());
    Ok(())
}

impl System for PspSystem {
    fn name(&self) -> &'static str {
        "PSP"
    }

    fn is_console(&self) -> bool {
        true
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME
    }

    fn can_load(&self, path: &Path) -> bool {
        is_psp_exe(path) || is_psp_disc(path)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if !file.is_dir() {
            if is_psp_exe(file) {
                let exe = file.path.clone();
                put_on_memstick(file, &exe)?;
                return Ok(true);
            }
            return Ok(is_psp_disc(file));
        }
        let mut disc = None;
        let mut exes: Vec<PathBuf> = vec![];
        walk_dir(&file.path.clone(), 0, |path, _ext, _header| {
            if is_psp_exe(path) {
                exes.push(path.to_owned());
            } else if disc.is_none() && is_psp_disc(path) {
                disc = Some(path.to_owned());
            }
            Ok(())
        })?;
        // A kxploit release has two EBOOTs; only the real one has its data
        // beside it.
        let exe = exes
            .iter()
            .find(|exe| has_data_beside(exe))
            .or(exes.first())
            .cloned();
        if let Some(disc) = disc {
            debug!("FMT: PSP disc {disc:?}");
            file.path = disc;
        } else if let Some(exe) = exe {
            put_on_memstick(file, &exe)?;
            debug!("FMT: PSP executable {:?}", file.path);
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}

#[cfg(test)]
#[path = "tests/psp_tests.rs"]
mod tests;
