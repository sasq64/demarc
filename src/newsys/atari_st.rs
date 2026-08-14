use anyhow::Result;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::debug;

use super::utils::{build_m3u, copy_dir_all};

use crate::{
    newsys::{utils::find_child, walk_dir},
    workfile::WorkFile,
};

use super::System;

const CORE_NAME_HATARI: &str = "hatari";
const GEMDOS_MAGIC: [u8; 2] = [0x60, 0x1a];

#[derive(Default)]
pub struct AtariStSystem {}

impl AtariStSystem {
    pub fn new() -> Self {
        Self {}
    }
}

/// Name given to the program copied into the drive's `AUTO` folder. TOS only
/// auto-starts `.PRG` files from there, so a `.TOS` or `.TTP` demo has to be
/// renamed to run — and 8.3 keeps GEMDOS from mangling the name.
const AUTO_PROGRAM: &str = "STARTME.PRG";

/// Where a release's own `AUTO` folder is moved when we start the program
/// ourselves. Its contents are kept, just not under the one name TOS runs on
/// boot.
const DISABLED_AUTO: &str = "NOAUTO";

/// `dir/name`, numbered (`NOAUTO2`, `NOAUTO3`, ...) until nothing of that name
/// is there already.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let mut path = dir.join(name);
    for n in 2.. {
        if !path.exists() {
            break;
        }
        path = dir.join(format!("{name}{n}"));
    }
    path
}

/// Stage `prg` and everything next to it as an Atari ST hard drive for hatari,
/// which mounts a host directory as GEMDOS drive C: and boots from it. Returns
/// the path to hand the core and the temp directory holding it, which the
/// caller has to keep alive for as long as the drive is needed.
///
/// The libretro core takes the drive as a `.gem` file whose name, minus that
/// extension, is the directory to mount — so the two are created side by side,
/// the `.gem` itself empty.
///
/// Booting from the drive runs `C:\AUTO\*.PRG`, so that is where `prg` goes. A
/// release that keeps its own program in `AUTO` already starts itself and is
/// left alone; any *other* `AUTO` folder is moved aside first, because what a
/// hard drive release carries there is usually the disk-swap stubs of its floppy
/// version, which stop the boot dead ("insert disk 1 and reboot").
///
/// Everything is copied into the temp directory rather than mounted in place:
/// the drive is writable from the emulator, and the `AUTO` folder is ours to
/// rearrange — neither is something to do to a directory of the user's own.
fn build_gemdos_drive(prg: &Path) -> Result<WorkFile> {
    let mut root = prg.parent().unwrap_or(Path::new("."));
    // A program already in an `AUTO` folder is started by the folder above it.
    let in_auto = root
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("auto"));
    if in_auto && let Some(parent) = root.parent() {
        root = parent;
    }

    let mut temp = WorkFile::new_dir()?;
    let drive = temp.join("harddrive");
    copy_dir_all(root, &drive)?;

    if !in_auto {
        if let Some(auto) = find_child(&drive, "auto").filter(|a| a.is_dir()) {
            let aside = free_name(&drive, DISABLED_AUTO);
            debug!("FMT: moving the release's AUTO folder aside to {aside:?}");
            fs::rename(&auto, &aside)?;
        }
        let auto = drive.join("AUTO");
        fs::create_dir_all(&auto)?;
        fs::copy(prg, auto.join(AUTO_PROGRAM))?;
    }

    let gem = temp.join("harddrive.gem");
    fs::write(&gem, [])?;
    temp.path = gem;
    Ok(temp)
}

impl System for AtariStSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_HATARI
    }

    fn name(&self) -> &'static str {
        "Atari ST"
    }

    fn default_tags(&self) -> HashMap<&str, &str> {
        [
            ("hatari_forcerefresh", "2"),
            ("hatari_start_in_mouse_mode", "false"),
            ("hatari_fastboot", "true"),
            ("hatari_video_crop_overscan", "false"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut exes = vec![];
        println!("LOAD {}: {file:?}", self.core_name());
        for (key, val) in self.default_tags() {
            file.set_tag(key, val);
        }

        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            println!("{path:?} {ext:?}");
            if ["msa", "st"].contains(&ext) {
                images.push(path.to_owned());
            } else if header[0..2] == GEMDOS_MAGIC {
                exes.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            if images.len() > 1 {
                let m3u = build_m3u(&images, file)?;
                file.path = m3u;
            } else {
                file.path = images[0].clone();
            }
        } else if !exes.is_empty() {
            let wf = build_gemdos_drive(&exes[0])?;
            file.path = wf.path;
            file.temp_dir = wf.temp_dir;
            return Ok(true);
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
