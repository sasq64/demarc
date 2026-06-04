use anyhow::Result;
use tracing::{debug, info};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum SystemType {
    C64,
    Amiga,
    Amstrad,
    AtariST,
    #[default]
    Unknown,
}

pub fn get_system_type(path: &Path) -> SystemType {
    let mut system_type = if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        match ext.as_str() {
            "adf" | "dms" | "ipf" | "hdf" | "lha" | "slave" => SystemType::Amiga,
            "d64" | "d81" => SystemType::C64,
            "dsk" => SystemType::Amstrad,
            "msa" | "st" => SystemType::AtariST,
            _ => SystemType::Unknown,
        }
    } else {
        SystemType::Unknown
    };
    if system_type == SystemType::Unknown {
        info!("Checking {:?}", path);
        if path.is_file() {
            let Ok(data) = fs::read(path) else {
                return SystemType::Unknown;
            };
            if data.len() >= 4 {
                let start = u16::from_le_bytes(data[..2].try_into().unwrap());
                if data[0..2] == [0x60, 0x1a] {
                    system_type = SystemType::AtariST;
                } else if data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
                    system_type = SystemType::Amiga;
                } else if (0x0400..=0x0801).contains(&start) {
                    system_type = SystemType::C64;
                }
            }
        }
    }
    system_type
}

#[derive(Debug, Default)]
pub struct WorkingFile {
    pub path: PathBuf,
    pub system_type: SystemType,
    pub settings: HashMap<String, String>,
    pub game_info: GameInfo,
    is_temp: bool,
}

impl Drop for WorkingFile {
    fn drop(&mut self) {
        if self.is_temp {
            // `path` may be the temp dir itself (Amiga) or a file inside it
            // (Atari disk image); either way remove the whole temp dir.
            let dir = if self.path.is_file() {
                self.path.parent().unwrap_or(&self.path)
            } else {
                &self.path
            };
            _ = fs::remove_dir_all(dir);
        }
    }
}

struct M3u {
    tags: HashMap<String, String>,
    files: Vec<PathBuf>,
}

fn parse_m3u(path: &Path) -> Result<M3u> {
    let contents = std::fs::read_to_string(path)?;
    let mut tags = HashMap::new();
    let mut files: Vec<PathBuf> = vec![];
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let mut remaining = rest;
            while let Some(eq) = remaining.find("=\"") {
                let key_start = remaining[..eq]
                    .rfind(|c: char| c.is_whitespace() || c == ',')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let key = remaining[key_start..eq].trim();
                let after_quote = &remaining[eq + 2..];
                let Some(end) = after_quote.find('"') else {
                    break;
                };
                let value = &after_quote[..end];
                if !key.is_empty() {
                    tags.insert(key.to_string(), value.to_string());
                }
                remaining = &after_quote[end + 1..];
            }
        } else if !line.starts_with('#') {
            files.push(line.into());
        }
    }
    Ok(M3u { tags, files })
}

#[derive(Default, Debug)]
pub struct GameInfo {
    pub title: String,
    pub group: String,
    pub year: String,
}

// let matches = path
//     .file_name()
//     .and_then(|n| n.to_str())
//     .is_some_and(|n| n.to_lowercase().contains(&name.to_lowercase()));
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

/// Build a bootable Atari ST FAT12 floppy image containing an `AUTO` directory
/// with `data` (a GEMDOS executable from `src`) copied into it, so it runs
/// automatically when the disk boots. Returns the path to the `.st` image,
/// which lives inside a fresh temp directory.
fn build_atari_auto_disk(data: &[u8]) -> Result<PathBuf> {
    use std::io::Write;

    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
    let img_path = target_dir.join("disk.st");

    let img = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&img_path)?;
    img.set_len(720 * 1024)?;
    fatfs::format_volume(
        &img,
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat12)
            .bytes_per_sector(512)
            .total_sectors(1440) // 720K = 1440 * 512
            .bytes_per_cluster(1024) // 2 sectors per cluster
            .max_root_dir_entries(112)
            .fats(2)
            .media(0xF9)
            .sectors_per_track(9)
            .heads(2)
            .volume_id(rand::random()),
    )?;

    let prog_name = "STARTME.PRG";

    let fs = fatfs::FileSystem::new(&img, fatfs::FsOptions::new())?;
    {
        let auto = fs.root_dir().create_dir("AUTO")?;
        let mut prog = auto.create_file(prog_name)?;
        prog.write_all(data)?;
        prog.flush()?;
    }
    fs.unmount()?;

    Ok(img_path)
}

fn handle_release(in_path: &Path, tags: &HashMap<String, String>) -> Result<WorkingFile> {
    let mut system_type = get_system_type(in_path);
    let mut path = in_path.to_owned();
    let mut tags = tags.clone();
    let mut is_temp = false;
    let game_info = GameInfo {
        title: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
        ..Default::default()
    };

    if path.is_dir() {
        if is_self_booting_dir(&path) {
            system_type = SystemType::Amiga;
            tags.insert("puae_use_whdload".into(), "disabled".into());
        } else {
            if has_matching(&path, ".slave").is_some() {
                system_type = SystemType::Amiga;
                tags.insert("puae_model".into(), "A1200".into());
                tags.insert("puae_use_whdload".into(), "enabled".into());
            }
            debug!("Checking");
            for f in fs::read_dir(&path)? {
                let f = f?;
                let t = get_system_type(&f.path());
                if t != SystemType::Unknown {
                    debug!("Found {t:?}");
                    path = f.path();
                    system_type = t;
                    break;
                }
            }
        }
    }

    if !path.is_dir() {
        let data = fs::read(&path)?;
        if data.len() >= 2 && data[0..2] == [0x60, 0x1a] {
            // GEMDOS executable: wrap it in a bootable Atari ST floppy image
            // with the program in the AUTO folder so it runs on boot.
            path = build_atari_auto_disk(&data)?;
            is_temp = true;
            system_type = SystemType::AtariST;
        } else if data.len() >= 2 && data[0..2] == [0x01, 0x08] {
            system_type = SystemType::C64;
        } else if data.len() >= 4 && data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
            let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
            let s_dir = target_dir.join("s");
            fs::create_dir(&s_dir)?;
            fs::write(s_dir.join("startup-sequence"), "amiga_file\n")?;
            fs::copy(&path, target_dir.join("amiga_file"))?;
            if std::fs::metadata(&path)?.len() > 850 * 1024 {
                tags.insert("puae_model".into(), "A1200".into());
            }
            path = target_dir;
            is_temp = true;
            tags.insert("puae_use_whdload".into(), "disabled".into());
            system_type = SystemType::Amiga;
        }
    };
    Ok(WorkingFile {
        system_type,
        path,
        settings: tags,
        game_info,
        is_temp,
    })
}

fn handle_m3u(in_path: &Path, tags: &HashMap<String, String>) -> Result<WorkingFile> {
    let mut title: String = "".into();
    let mut group: String = "".into();
    let mut year: String = "".into();
    let mut system_type = SystemType::Unknown;
    let mut tags = tags.clone();
    let m3u = parse_m3u(in_path).unwrap();
    info!("{:?}", m3u.tags);
    if let Some(t) = m3u.tags.get("title") {
        title = t.clone();
    }
    if let Some(t) = m3u.tags.get("group") {
        group = t.clone();
    }
    if let Some(t) = m3u.tags.get("year") {
        year = t.clone();
    }
    for (key, val) in m3u.tags {
        if key.starts_with("vice_") || key.starts_with("puae_") || key.starts_with("hatari_") {
            tags.insert(key, val);
        }
    }

    if m3u.files.is_empty() {
        let mut wf = handle_release(in_path.parent().unwrap(), &tags)?;
        wf.game_info.title = title;
        wf.game_info.group = group;
        wf.game_info.year = year;
        return Ok(wf);
    }

    if let Some(path) = m3u.files.first() {
        let real_path = in_path.parent().unwrap().join(path);
        system_type = get_system_type(&real_path);
    }

    let game_info = GameInfo { title, group, year };

    Ok(WorkingFile {
        system_type,
        path: in_path.to_owned(),
        settings: tags,
        game_info,
        is_temp: false,
    })
}

/// Options
/// - Vaild M3u (for System) with tags
/// - Metadata only M3U (Unknown system) with tags-> Redirect to parent
/// - Direct file, no meta data, with tags
pub fn handle_file(in_path: &Path, tags: &HashMap<String, String>) -> Result<WorkingFile> {
    if let Some(ext) = in_path.extension()
        && ext == "m3u"
    {
        handle_m3u(in_path, tags)
    } else {
        handle_release(in_path, tags)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atari_exe() {
        let assets = Path::new("demos").to_owned();
        let wf = handle_file(&assets.join("natrium.prg"), &HashMap::new()).unwrap();
        assert_eq!(wf.system_type, SystemType::AtariST);
        println!("{:?}", wf);
    }
}
