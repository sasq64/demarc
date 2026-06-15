use anyhow::Result;
use tracing::{debug, info, warn};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use crate::retro::system_dir;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum SystemType {
    C64,
    Amiga,
    Amstrad,
    AtariST,
    Megadrive,
    Atari2600,
    SuperNintendo,
    ZXSpectrum,
    AtariXL,
    #[default]
    Unknown,
}

fn check_reset_vector(data: &[u8]) -> bool {
    let len = data.len();
    if len < 4 {
        return false;
    }

    // Reset vector is at the last 4 bytes: NMI, RESET, IRQ/BRK
    // For a cart ending at $FFFF, reset vector is at offset len-4+2
    let reset_lo = data[len - 4 + 2] as u16;
    let reset_hi = data[len - 4 + 3] as u16;
    let reset_addr = (reset_hi << 8) | reset_lo;

    // Should point into the high ROM bank, typically $F000–$FFFF
    // (or $E000–$FFFF for 8KB, etc.)
    let bank_start = 0x10000u32 - len as u32;
    reset_addr as u32 >= bank_start
}

pub fn get_system_type(path: &Path) -> SystemType {
    let mut system_type = if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        match ext.as_str() {
            "adf" | "dms" | "ipf" | "hdf" | "lha" | "slave" => SystemType::Amiga,
            "d64" | "d81" | "crt" | "g64" | "x64" => SystemType::C64,
            "dsk" => SystemType::Amstrad,
            "msa" | "st" => SystemType::AtariST,
            "a26" => SystemType::Atari2600,
            "tap" | "scl" | "trd" => SystemType::ZXSpectrum,
            "smc" | "sfc" => SystemType::SuperNintendo,
            "atr" | "xex" | "atx" => SystemType::AtariXL,
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
            let l = data.len();
            if data.len() >= 4 {
                if l >= 0x200
                    && std::str::from_utf8(&data[0x100..0x110])
                        .unwrap_or("")
                        .starts_with("SEGA ")
                {
                    system_type = SystemType::Megadrive;
                } else if l.is_power_of_two()
                    && (2048..=32768).contains(&l)
                    && check_reset_vector(&data)
                {
                    system_type = SystemType::Atari2600;
                } else if data[0..2] == [0x60, 0x1a] {
                    system_type = SystemType::AtariST;
                } else if data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
                    system_type = SystemType::Amiga;
                } else if (0x0400..=0x0801).contains(&u16::from_le_bytes(
                    data[..2].try_into().unwrap_or_default(),
                )) {
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
            // `path` may be the temp dir itself (Amiga), a file inside it (Atari
            // disk image), or a subdirectory of it (zip with a single top-level
            // dir). Walk up to the `demarc-` temp root and remove the whole tree.
            let mut dir = self.path.as_path();
            while dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| !n.starts_with("demarc-"))
            {
                match dir.parent() {
                    Some(parent) => dir = parent,
                    None => break,
                }
            }
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

/// Copy `files` into a fresh temp directory and write a `demo.m3u` that
/// references each copied file by name. Returns the path to the `.m3u`, which
/// lives inside the temp directory alongside the copied files.
fn build_m3u(files: &[PathBuf]) -> Result<PathBuf> {
    use std::io::Write;

    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();

    let mut contents = String::from("#EXTM3U\n");
    for file in files {
        let name = file
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
        fs::copy(file, target_dir.join(name))?;
        contents.push_str(&name.to_string_lossy());
        contents.push('\n');
    }

    let m3u_path = target_dir.join("demo.m3u");
    let mut m3u = fs::File::create(&m3u_path)?;
    m3u.write_all(contents.as_bytes())?;
    m3u.flush()?;

    Ok(m3u_path)
}

/// Sort disk images so that the most "main" disk comes first. Ordering rules:
/// 1. Files whose stem ends in a digit (a digit right next to the extension dot)
///    come first, e.g. `disk3.d64`.
/// 2. Files that contain a digit somewhere else come next, e.g. `disk2_extra.d64`.
/// 3. Files with no digit at all come last, e.g. `anything.d64`.
///
/// Within each group files are ordered by name for a stable, predictable result.
fn sort_disks(files: &mut [PathBuf]) {
    fn rank(path: &Path) -> u8 {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if stem.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            0
        } else if stem.chars().any(|c| c.is_ascii_digit()) {
            1
        } else {
            2
        }
    }

    files.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
}

/// True if `path` is a regular file beginning with the ZIP local-file-header
/// magic (`PK\x03\x04`).
fn is_zip_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && magic == [0x50, 0x4b, 0x03, 0x04]
}

/// Extract `path` (a zip archive) into a fresh temp directory and return that
/// directory.
fn unzip_to_temp(path: &Path) -> Result<PathBuf> {
    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
    let mut archive = zip::ZipArchive::new(fs::File::open(path)?)?;
    archive.extract(&target_dir)?;

    // If the archive contained a single top-level directory, descend into it so
    // the release-detection logic sees the actual files, not the wrapper dir.
    let entries: Vec<PathBuf> = fs::read_dir(&target_dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    if let [only] = entries.as_slice()
        && only.is_dir()
    {
        return Ok(only.clone());
    }
    Ok(target_dir)
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
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

    if path.is_file() && is_zip_file(&path) {
        debug!("FMT: zip archive");
        path = unzip_to_temp(&path)?;
        is_temp = true;
        system_type = get_system_type(&path);
    }
    let mut copy_all = false;

    if path.is_dir() {
        if is_self_booting_dir(&path) {
            debug!("FMT: Amiga self-booting");
            system_type = SystemType::Amiga;
            tags.insert("puae_use_whdload".into(), "disabled".into());
        } else if has_matching(&path, ".slave").is_some() {
            debug!("FMT: Amiga WHDLoad");
            system_type = SystemType::Amiga;
            tags.insert("puae_model".into(), "A1200".into());
            tags.insert("puae_use_whdload".into(), "enabled".into());
        } else {
            let mut files = vec![];
            let mut one_file = None;
            for f in fs::read_dir(&path)? {
                let f = f?;
                let t = get_system_type(&f.path());
                debug!("Checking {:?} => {:?}", f.path(), t);
                if t != SystemType::Unknown {
                    if one_file.is_none() {
                        one_file = Some(f.path());
                        system_type = t;
                    }
                    let ext = f
                        .path()
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or("".into())
                        .to_lowercase();
                    if ext == "d64" || ext == "adf" || ext == "atr" {
                        debug!("Found {t:?}");
                        files.push(f.path());
                        path = f.path();
                        system_type = t;
                    }
                }
            }
            if files.len() > 1 {
                sort_disks(&mut files);
                path = build_m3u(&files)?;
                is_temp = true;
            } else if let Some(f) = one_file {
                path = f;
                copy_all = true;
                //let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
                //copy_dir_all(&path, &target_dir)?;
                //path = target_dir;
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
            debug!("FMT: Amiga exe: {path:?}");
            let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?.keep();
            let s_dir = target_dir.join("s");
            fs::create_dir(&s_dir)?;
            let c_dir = target_dir.join("c");
            fs::create_dir(&c_dir)?;
            fs::copy(system_dir().join("c").join("echo"), c_dir.join("echo"))?;
            if copy_all {
                let name = path.file_name().unwrap().to_str().unwrap();
                debug!("COPY ALL: {name}");
                fs::write(
                    s_dir.join("startup-sequence"),
                    format!("echo \"Loading...\"\n{name}\n"),
                )?;
                copy_dir_all(path.parent().unwrap(), &target_dir)?;
            } else {
                fs::write(
                    s_dir.join("startup-sequence"),
                    "echo \"Loading...\"\namiga_file\n",
                )?;
                fs::copy(&path, target_dir.join("amiga_file"))?;
            }
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

    let m3u = parse_m3u(in_path)?;
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
        debug!("FMT: M3U metadata only");
        let mut wf = handle_release(in_path.parent().unwrap(), &tags)?;
        wf.game_info.title = title;
        wf.game_info.group = group;
        wf.game_info.year = year;
        return Ok(wf);
    }

    if let Some(path) = m3u.files.first() {
        let real_path = in_path.parent().unwrap().join(path);
        system_type = get_system_type(&real_path);
        debug!("FMT: M3U {system_type:?}");
    }
    if !tags.contains_key("vice_jiffydos") {
        tags.insert("vice_jiffydos".into(), "enabled".into());
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
    info!("Handle {in_path:?}");
    if let Some(ext) = in_path.extension()
        && ext == "m3u"
    {
        handle_m3u(in_path, tags)
    } else {
        handle_release(in_path, tags)
    }
}

/// Recursively collect all detected emulator files under `dir` into `out`.
pub fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    // if dir.is_dir() && is_self_booting_dir(dir) {
    //     println!("SELF BOOTING");
    //     out.push(dir.to_owned());
    //     return;
    // }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn!("Failed to read directory {}: {err}", dir.display());
            return;
        }
    };
    let mut files = vec![];
    let mut dirs = vec![];
    let mut found_type = SystemType::Unknown;
    let mut mixed = false;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
        {
            out.push(path);
            return;
        } else {
            let t = get_system_type(&path);
            if t != SystemType::Unknown {
                if found_type != SystemType::Unknown && found_type != t {
                    mixed = true;
                }
                found_type = t;
                files.push(path);
            }
        }
    }

    if mixed {
        out.extend(files.iter().map(|f| f.into()));
    } else if found_type != SystemType::Unknown {
        out.push(dir.to_owned());
        return;
    }
    for dir in dirs {
        collect_files(&dir, out);
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

    #[test]
    fn amiga_m3u() {
        let assets = Path::new("demos").to_owned();
        let wf = handle_file(&assets.join("nexus7").join("demo.m3u"), &HashMap::new()).unwrap();
        println!("{:?}", wf);
        assert_eq!(wf.settings.get("puae_model").unwrap(), "A1200");
        assert_eq!(wf.system_type, SystemType::Amiga);
    }

    #[test]
    fn amiga_exe() {
        let assets = Path::new("demos").to_owned();
        let wf = handle_file(&assets.join("o2-intro").join("o2intro"), &HashMap::new()).unwrap();
        println!("{:?}", wf);
        assert_eq!(wf.system_type, SystemType::Amiga);
        assert!(wf.path.join("s").exists());
        assert!(wf.path.join("s/startup-sequence").exists());
        assert!(wf.path.join("amiga_file").exists());
    }

    #[test]
    fn collect_amiga() {
        let assets = Path::new("demos").to_owned();
        let mut out = vec![];
        collect_files(&assets.join("o2-intro"), &mut out);
        println!("{:?}", out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn all_demos() {
        let assets = Path::new("demos").to_owned();
        let mut out = vec![];
        collect_files(&assets, &mut out);
        println!("{:?}", out);
        assert_eq!(out.len(), 6);
    }
}
