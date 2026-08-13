use anyhow::Result;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::debug;

use super::utils::{build_m3u, copy_dir_all, has_any_extension, read_header};

use crate::{frontend::system_dir, newsys::walk_dir, workfile::WorkFile};

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
        let mut images = vec![];
        let mut exes = vec![];
        println!("LOAD Amiga: {file:?}");
        for (key, val) in self.default_tags() {
            file.set_tag(key, val);
        }

        let mut is_dir = false;
        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            println!("{path:?} {ext:?}");
            if ["adf", "dms"].contains(&ext) {
                images.push(path.to_owned());
            } else if ext == "slave" {
                file.tags.insert("puae_model".into(), "A1200".into());
                file.tags
                    .insert("puae_use_whdload".into(), "enabled".into());
                is_dir = true;
            } else if header[0..4] == [0x00, 0x00, 0x03, 0xF3] {
                exes.push(path.to_owned());
            } else if let Some(f) = file.file_name()
                && f == "startup-sequence"
            {
                // Auto-booting
                file.tags
                    .insert("puae_use_whdload".into(), "disabled".into());
                is_dir = true;
            }
            Ok(())
        })?;

        if is_dir {
            return Ok(true);
        }

        if !images.is_empty() {
            if images.len() > 1 {
                let m3u = build_m3u(&images, file)?;
                file.path = m3u;
            } else {
                file.path = images[0].clone();
            }
        } else if !exes.is_empty() {
            file.path = exes[0].clone();
            handle_exe(file, false);
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
