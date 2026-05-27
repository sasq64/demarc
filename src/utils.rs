use anyhow::Result;
use anyhow::bail;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum SystemType {
    C64,
    Amiga,
    #[default]
    Unknown,
}

pub fn get_sytem_type(path: &Path) -> SystemType {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        match ext {
            "adf" => SystemType::Amiga,
            "prg" | "d64" => SystemType::C64,
            _ => SystemType::Unknown,
        }
    } else {
        SystemType::Unknown
    }
}

#[derive(Debug, Default)]
pub struct WorkingFile {
    pub path: PathBuf,
    pub system_type: SystemType,
    pub settings: HashMap<String, String>,
    is_temp: bool,
}

impl Drop for WorkingFile {
    fn drop(&mut self) {
        if self.is_temp {
            _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Find a direct child of `dir` whose name matches `name` case-insensitively.
/// Amiga volumes are case-insensitive, so a host directory meant to act as one
/// may use any casing (e.g. `S/Startup-Sequence`).
fn find_child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
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
/// i.e. it can boot on its own as a hard drive without the WHDLoad helper.
fn is_self_booting_dir(game: &Path) -> bool {
    find_child_ci(game, "s")
        .is_some_and(|s_dir| find_child_ci(&s_dir, "startup-sequence").is_some())
}
pub fn handle_file(in_path: &Path, tags: &HashMap<String, String>) -> Result<WorkingFile> {
    let mut path = in_path.to_owned();
    let mut settings = tags.clone();
    let mut is_temp = false;
    if !path.exists() {
        bail!("No such file");
    }
    let mut system_type = get_sytem_type(in_path);
    if system_type == SystemType::Unknown {
        if path.is_dir() {
            if is_self_booting_dir(&path) {
                system_type = SystemType::Amiga;
                settings.insert("puae_use_whdload".into(), "disabled".into());
            } else {
                //if find_file(&path, ".slave") {
                system_type = SystemType::Amiga;
                settings.insert("puae_use_whdload".into(), "esabled".into());
            }
        } else {
            println!("READ");
            let data = fs::read(&path)?;
            if data.len() >= 2 && data[0..2] == [0x01, 0x08] {
                system_type = SystemType::C64;
            } else if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
                let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
                let s_dir = target_dir.join("s");
                fs::create_dir(&s_dir)?;
                fs::write(s_dir.join("startup-sequence"), "amiga_file\n")?;
                fs::copy(&path, target_dir.join("amiga_file"))?;
                if std::fs::metadata(&path)?.len() > 1024 * 1024 {
                    settings.insert("puae_model".into(), "A1200".into());
                }
                path = target_dir;
                is_temp = true;
                settings.insert("puae_use_whdload".into(), "disabled".into());
                system_type = SystemType::Amiga;
            }
        }
    }
    Ok(WorkingFile {
        system_type,
        path,
        settings,
        is_temp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amiga_exe() {
        let assets = Path::new("assets").to_owned();
        let mut wf = handle_file(&assets.join("lemon.exe"), &HashMap::new()).unwrap();

        assert_eq!(wf.system_type, SystemType::Amiga);
        println!("{:?}", wf);
    }
}
