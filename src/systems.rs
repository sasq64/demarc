use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;
use tracing::{debug, info};

use crate::{
    Args, CbmSystem,
    emu_file::EmuFile,
    utils::{cue_has_data_track, is_gba_rom, is_snes_rom, read_header},
};

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
    Tic80,
    Pico8,
    Flash,
    Gameboy,
    Gba,
    Psx,
    NeoGeo,
    Ilbm,
    Degas,
    Gfx,
    #[default]
    Unknown,
}

impl SystemType {
}

#[derive(Default, Debug, Clone)]
pub struct GameInfo {
    pub title: String,
    pub group: String,
    pub year: String,
    pub typ: String,
}

#[derive(Debug, Default)]
pub struct WorkingFile {
    pub path: PathBuf,
    pub system_type: SystemType,
    pub settings: HashMap<String, String>,
    pub game_info: GameInfo,
    /// The temp directory `path` was built in, when it was built at all. `path`
    /// may be the directory itself (Amiga), a file inside it (Atari disk image)
    /// or a file in a subdirectory of it (zip with a single top-level dir); in
    /// every case holding the [`TempDir`] here is what keeps it on disk, and
    /// dropping the `WorkingFile` removes the whole tree.
    #[allow(dead_code)]
    pub temp_dir: Option<TempDir>,
}

pub fn get_memory(work_file: &WorkingFile) -> String {
    let tags = &work_file.settings;
    let reu = tags.get("vice_ram_expansion_unit");
    let a1200 = tags.get("puae_model").is_some_and(|v| v == "A1200");
    let chip = tags
        .get("puae_chipmem_size")
        .map(|c| c.parse::<u32>().unwrap_or_default())
        .unwrap_or(if a1200 { 4 } else { 1 })
        * 512;

    //let ste = tags.get("hatari_machinetype").is_some_and(|v| v == "ste");
    match work_file.system_type {
        SystemType::C64 => {
            if let Some(reu) = reu {
                format!("64K + REU {}", reu)
            } else {
                "64K".to_string()
            }
        }
        SystemType::Amiga => format!("CHIP:{}K", chip),
        SystemType::Amstrad => "128K".to_string(),
        SystemType::Megadrive => "64K + VRAM:64K".to_string(),
        SystemType::ZXSpectrum => "128K".to_string(),
        SystemType::AtariST => "".to_string(),
        SystemType::Atari2600 => "128B".to_string(),
        SystemType::SuperNintendo => "128K".to_string(),
        SystemType::AtariXL => "Atari XL".to_string(),
        SystemType::Tic80 => "272KB".to_string(),
        SystemType::Pico8 => "?".to_string(),
        SystemType::Flash => "?".to_string(),
        _ => "?".to_string(),
    }
}

pub fn get_system_name(work_file: &WorkingFile) -> String {
    system_name(work_file.system_type, &work_file.settings)
}

/// The display name of a system, refined by the tags that distinguish its
/// variants (an STE from an ST, an AGA Amiga from an A500). Takes the parts
/// rather than a [`WorkingFile`] so it also works for an
/// [`EmuFile`](crate::files::EmuFile) that hasn't been prepared for loading yet.
pub fn system_name(system_type: SystemType, tags: &HashMap<String, String>) -> String {
    let ste = tags.get("hatari_machinetype").is_some_and(|v| v == "ste");
    let a1200 = tags.get("puae_model").is_some_and(|v| v == "A1200");
    let mut base = match system_type {
        SystemType::C64 => "C64",
        SystemType::Amiga => "Amiga",
        SystemType::Amstrad => "Amstrad CPC",
        SystemType::Megadrive => "Megadrive",
        SystemType::ZXSpectrum => "ZX Spectrum",
        SystemType::AtariST => {
            if ste {
                "Atari STE"
            } else {
                "Atari ST"
            }
        }
        SystemType::Atari2600 => "Atari 2600",
        SystemType::SuperNintendo => "SNES",
        SystemType::AtariXL => "Atari XL",
        SystemType::Tic80 => "Tic-80",
        SystemType::Pico8 => "Pico8",
        SystemType::Flash => "Flash",
        SystemType::Gameboy => "Gameboy",
        SystemType::Gba => "GBA",
        SystemType::Psx => "PlayStation",
        SystemType::NeoGeo => "Neo Geo",
        SystemType::Ilbm => "Amiga Gfx",
        SystemType::Degas => "Atari Gfx",
        SystemType::Gfx => "Gfx",
        SystemType::Unknown => "Unknown",
    }
    .to_string();
    if system_type == SystemType::Amiga {
        if a1200 {
            base += " (AGA)";
        } else {
            base += " 500";
        }
    }
    base
}

#[expect(dead_code)]
pub fn get_full_info(work_file: &WorkingFile) -> String {
    let system = get_system_name(work_file);
    let ram = get_memory(work_file);
    let len = fs::metadata(&work_file.path).unwrap().len();

    let GameInfo {
        title,
        group,
        year,
        typ: _,
    } = &work_file.game_info;
    let year = if year.is_empty() {
        "".into()
    } else {
        format!(" ({year})")
    };

    format!("\"{title}\"\n{group}\n{system}{year}\nMem: {ram}\n Size: {len}")
}

pub fn get_info_text(work_file: &EmuFile, tags: &HashMap<String, String>) -> String {
    let system = tags.get("system").cloned().unwrap_or("???".to_string()); //get_system_name(work_file);
    let GameInfo {
        title,
        group,
        year,
        typ,
    } = &work_file.game_info;
    let year = if year.is_empty() {
        "".into()
    } else {
        format!(" ({year})")
    };
    let desc = if typ.is_empty() { &system } else { &typ };

    format!("\"{title}\"\n{group}\n{desc}{year}")
}

/// The branch instruction every GEMDOS executable starts with — an Atari ST
/// program is recognized by this, whatever it is named (`.prg`, `.tos`, `.ttp`
/// or nothing at all).
pub const GEMDOS_MAGIC: [u8; 2] = [0x60, 0x1a];

pub fn get_system_type(path: &Path) -> SystemType {
    let ext = if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        ext.to_lowercase()
    } else {
        "".to_string()
    };
    let mut system_type = match ext.as_str() {
        "adf" | "dms" | "ipf" | "hdf" | "slave" => SystemType::Amiga,
        "d64" | "d81" | "crt" | "g64" | "x64" | "t64" | "lnx" | "p00" => SystemType::C64,
        "dsk" => SystemType::Amstrad,
        "smd" | "gen" | "32x" => SystemType::Megadrive,
        "msa" | "st" => SystemType::AtariST,
        "a26" => SystemType::Atari2600,
        "tap" | "scl" | "trd" => SystemType::ZXSpectrum,
        // `.swc`/`.fig` are copier dumps — same ROM behind a 512-byte header.
        "smc" | "sfc" | "swc" | "fig" => SystemType::SuperNintendo,
        "atr" | "xex" | "atx" => SystemType::AtariXL,
        "tic80" | "tic" => SystemType::Tic80,
        "p8" => SystemType::Pico8,
        "gb" | "gbc" | "cgb" => SystemType::Gameboy,
        "gba" | "agb" => SystemType::Gba,
        // CD-image containers. The sheet points at the bulk track data, so it —
        // not the `.bin` — is what gets loaded.
        "chd" | "pbp" | "ccd" | "toc" | "psx" => SystemType::Psx,
        // A `.cue` is just as likely to be an audio-CD rip sitting in a music
        // library, so it only counts as PlayStation if it has a data track.
        "cue" if cue_has_data_track(path) => SystemType::Psx,
        "iso" => SystemType::Psx,
        "swf" => SystemType::Flash,
        "iff" | "ilbm" | "lbm" => SystemType::Ilbm,
        // DEGAS low resolution, plain and compressed. The medium- and
        // high-resolution variants (.pi2/.pi3, .pc2/.pc3) are far rarer and are
        // left unclaimed for now.
        "pi1" | "pc1" => SystemType::Degas,
        "gif" | "png" | "bmp" | "jpg" | "jpeg" => SystemType::Gfx,
        "neo" => SystemType::NeoGeo,
        _ => SystemType::Unknown,
    };
    if system_type == SystemType::Unknown {
        info!("Checking {:?}", path);
        if path.is_file() {
            // Only the first 0x200 bytes are ever inspected; CD tracks and other
            // bulk images are far too big to pull into memory just to sniff.
            let Ok(data) = read_header(path, 0x200) else {
                return SystemType::Unknown;
            };
            let Ok(l) = fs::metadata(path).map(|m| m.len() as usize) else {
                return SystemType::Unknown;
            };
            if data.len() >= 4 {
                if data.len() >= 0x200
                    && std::str::from_utf8(&data[0x100..0x110])
                        .unwrap_or("")
                        .starts_with("SEGA ")
                {
                    system_type = SystemType::Megadrive;
                } else if l >= 8 && &data[0..8] == b"PS-X EXE" {
                    system_type = SystemType::Psx;
                } else if is_gba_rom(&data) {
                    system_type = SystemType::Gba;
                } else if is_snes_rom(path) {
                    // Before the 2600, since a headerless 32K ROM is the size
                    // of a big cartridge for that machine too.
                    system_type = SystemType::SuperNintendo;
                } else if l.is_power_of_two() && (2048..=32768).contains(&l) && ext == "bin" {
                    system_type = SystemType::Atari2600;
                } else if data[0..2] == GEMDOS_MAGIC {
                    system_type = SystemType::AtariST;
                } else if data[0..4] == [0x00, 0x00, 0x03, 0xF3] {
                    system_type = SystemType::Amiga;
                } else if l >= 12 && &data[0..4] == b"FORM" && &data[8..12] == b"ILBM" {
                    system_type = SystemType::Ilbm;
                } else if crate::degas::is_degas(&data, l) {
                    system_type = SystemType::Degas;
                } else if matches!(&data[0..3], b"FWS" | b"CWS" | b"ZWS") {
                    // Flash SWF signatures: uncompressed / zlib / LZMA.
                    system_type = SystemType::Flash;
                } else if (0x0400..=0x0801).contains(&u16::from_le_bytes(
                    data[..2].try_into().unwrap_or_default(),
                )) && ext == "prg"
                {
                    system_type = SystemType::C64;
                }
            }
        }
    }
    debug!("Found {system_type:?}");
    system_type
}

pub fn tags_from_args(args: &Args) -> HashMap<String, String> {
    let mut tags = HashMap::new();
    let mut set_var = |name: &str, val: &str| tags.insert(name.into(), val.into());

    set_var("latency", &args.latency.to_string());
    set_var("fuse_machine", "Spectrum 128K");
    set_var("atari800_ntscpal", "PAL");
    //set_var("atari800_system", "Modern XL/XE(576K)");
    set_var("atari800_system", "Modern XL/XE(1088K)");
    set_var(
        "cbm_variant",
        match args.cbm_variant {
            CbmSystem::C64 => "c64",
            CbmSystem::C128 => "c128",
            CbmSystem::Dtv => "dtv",
            CbmSystem::C16 => "c16",
            CbmSystem::VIC20 => "vic20",
        },
    );
    if args.db.is_some() {
        set_var("puae_model", "date");
    }
    if args.aga {
        set_var("puae_model", "A1200");
    }
    if args.ste {
        set_var("hatari_machinetype", "ste");
        set_var("hatari_ramsize", "4");
    }

    if args.xmem {
        set_var("hatari_ramsize", "8");
        set_var("puae_z3mem_size", "128");
        set_var("puae_chipmem_size", "4");
        set_var("puae_fastmem_size", "8");
    }
    if args.fast {
        set_var("hatari_ramsize", "8");
        set_var("puae_z3mem_size", "128");
        set_var("puae_fpu_model", "68882");
        set_var("puae_cpu_model", "68030");
        // set_var("puae_cpu_throttle", "10000");
        set_var("puae_cpu_compatibility", "exact");
    }

    if args.silent_drive {
        set_var("puae_floppy_sound", "100");
        set_var("vice_drive_sound_emulation", "disabled");
        set_var("cap32_floppy_sound", "disabled");
    }

    if args.fast_load {
        //set_var("vice_jiffydos", "enabled");
        set_var("puae_floppy_speed", "0");
        set_var("fast_load", "on");
    }

    if args.reu {
        set_var("vice_ram_expansion_unit", "16384kB");
    }

    if args.color_cycle {
        set_var("color_cycle", "enabled");
    }
    for opt in &args.extra_options {
        if let Some((key, val)) = opt.split_once("=") {
            set_var(key.trim(), val.trim());
        }
    }
    tags
}
