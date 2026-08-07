use anyhow::Result;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::debug;

use super::utils::{build_m3u, copy_dir_all, get_disk_images, has_any_extension, read_header};

use crate::{frontend::system_dir, workfile::WorkFile};

use super::System;

const CORE_NAME_UAE: &str = "puae";
pub struct AmigaSystem {}

impl AmigaSystem {}

fn has_matching(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(&name.to_lowercase()));
        matches.then_some(path)
    })
}

fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name));
        matches.then_some(path)
    })
}

/// True if `game` is a directory containing an `s/startup-sequence` boot script,
fn is_self_booting_dir(game: &Path) -> bool {
    find_child(game, "s").is_some_and(|s_dir| find_child(&s_dir, "startup-sequence").is_some())
}

fn handle_exe(wf: &mut WorkFile, copy_all: bool) -> Result<()> {
    debug!("FMT: Amiga exe: {wf:?}");
    if std::fs::metadata(&wf)?.len() > 850 * 1024 {
        wf.set_tag("puae_model", "A1200");
    }

    let target_dir = WorkFile::new_dir()?;
    let s_dir = target_dir.join("s");
    fs::create_dir(&s_dir)?;
    let c_dir = target_dir.join("c");
    fs::create_dir(&c_dir)?;
    fs::copy(system_dir().join("c").join("echo"), c_dir.join("echo"))?;
    let mut text: String = "".into();
    let model = wf.get_tag("puae_model", "");
    if model == "A1200" || model == "A4000" {
        fs::copy(
            system_dir().join("c").join("SetPatch"),
            c_dir.join("SetPatch"),
        )?;
        text += "SetPatch QUIET\n";
    }
    if copy_all {
        let name = wf.file_name().unwrap().to_str().unwrap();
        text += &format!("echo \"Loading...\"\n{name}\n");
    } else {
        text += "echo \"Loading...\"\namiga_file\n";
    }
    fs::write(s_dir.join("startup-sequence"), text)?;
    if copy_all {
        copy_dir_all(wf.parent().unwrap(), &target_dir)?;
    } else {
        fs::copy(&wf, target_dir.join("amiga_file"))?;
    }
    wf.path = target_dir.path;
    wf.temp_dir = target_dir.temp_dir;
    wf.set_tag("puae_use_whdload", "disabled");

    Ok(())
}

impl System for AmigaSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["adf", "dms", "slave", "ips"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_UAE
    }

    fn name(&self) -> &'static str {
        "Amiga"
    }

    fn default_tags(&self) -> HashMap<&str, &str> {
        [
            ("puae_model", "A500"),
            ("puae_crop", "smaller"),
            ("puae_horizontal_pos", "-5"),
            ("puae_mapper_mouse_toggle", "---"),
        ]
        .into()
    }

    fn can_load(&self, path: &Path) -> bool {
        if has_any_extension(path, &["dms", "adf", "ips"]) {
            return true;
        }
        let data = read_header(path, 4).unwrap_or_default();
        data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x03, 0xF3]
    }
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        println!("LOAD Amiga: {file:?}");
        for (key, val) in self.default_tags() {
            file.set_tag(key, val);
        }
        if file.is_dir() {
            if is_self_booting_dir(&file) {
                debug!("FMT: Amiga self-booting");
                file.set_tag("puae_use_whdload", "disabled");
            } else if has_matching(&file, ".slave").is_some() {
                debug!("FMT: Amiga WHDLoad");
                file.set_tag("puae_model", "A1200");
                file.set_tag("puae_use_whdload", "enabled");
            } else {
                let scanned = get_disk_images(file, &["adf", "dms", "ips"])?;
                println!("DIR {scanned:?}");
                if !scanned.is_empty() {
                    let m3u = build_m3u(&scanned, file)?;
                    file.path = m3u;
                } else {
                    if let Some(path) = self.get_first_file(file)? {
                        file.path = path;
                        handle_exe(file, true);
                    } else {
                        return Ok(false);
                    }
                }
            }
        } else {
            if self.can_load(file) {
                handle_exe(file, false);
            } else {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
