use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};
use tracing::{debug, info};

use super::utils::{copy_dir_all, has_any_extension, read_header};

use crate::{
    Args,
    frontend::system_dir,
    libloader,
    newsys::{collect_disk_images, utils::has_extension, walk_dir},
    retro_emu::{Backend, RetroCoreThreaded},
    workfile::WorkFile,
};

use super::System;

const CORE_NAME_UAE: &str = "puae";
/// Amiberry's libretro port. The libretro buildbot does not ship it, so it is
/// downloaded from its own release instead (see `ALT_SOURCES`) — or taken from
/// `$DEMARC_CORE_DIR` when a local build is being tested (see AMIBERRY.md).
const CORE_NAME_AMIBERRY: &str = "amiberry";

/// Which core the `amiga_core` option asks for. Anything else, including the
/// option being unset, is the default p-uae core.
fn use_amiberry(file: &WorkFile) -> bool {
    file.get_meta("amiga_core", "").contains("amiberry")
}

/// Rewrite the `puae_*` options demarc emits into the `amiberry_*` options the
/// Amiberry libretro core understands, best effort.
///
/// Amiberry silently drops any option it doesn't recognise, so without this
/// every demo boots as a default OCS A500 / 68000 / KS 1.3 regardless of the
/// puae config — AGA included (see `docs/AMIBERRY.md`, "Integration gaps"). It
/// can't be a pure rename: several puae options (`puae_fpu_model`,
/// `puae_fastmem_size`, `puae_chipmem_size`, the display and floppy tweaks) have
/// no Amiberry equivalent and are simply left behind. An already-present
/// `amiberry_*` key (e.g. hand-written in an m3u) always wins over the
/// translation.
fn puae_to_amiberry(meta: &mut HashMap<String, String>) {
    // Direct renames: the value space is the same in both cores.
    for (puae, amiberry) in [
        ("puae_model", "amiberry_model"),
        ("puae_cpu_model", "amiberry_cpu_model"),
        ("puae_z3mem_size", "amiberry_z3mem_size"),
        ("puae_kickstart", "amiberry_kickstart"),
        ("puae_video_standard", "amiberry_video_standard"),
    ] {
        if let Some(v) = meta.get(puae).cloned() {
            meta.entry(amiberry.to_string()).or_insert(v);
        }
    }

    // Amiberry exposes no Zorro II fastmem option, but its Zorro III fast RAM
    // (`amiberry_z3mem_size`, which we added) covers the same need. Fold
    // `puae_fastmem_size` into it when the config didn't already ask for Z3.
    if !meta.contains_key("amiberry_z3mem_size")
        && let Some(v) = meta.get("puae_fastmem_size").cloned()
    {
        meta.insert("amiberry_z3mem_size".into(), v);
    }

    // p-uae runs the CPU unthrottled by default; Amiberry throttles it to the
    // modelled machine's clock unless told otherwise, which crawls on an
    // accelerated model (see `docs/AMIBERRY.md`, "Starstruck"). Match p-uae's
    // behaviour for 68020+ configs via the `amiberry_cpu_speed` option we added.
    let accelerated = meta
        .get("amiberry_cpu_model")
        .is_some_and(|m| m != "68000" && m != "68010");
    if accelerated {
        meta.entry("amiberry_cpu_speed".into())
            .or_insert("max".into());
    }
}

/// First longword of an AmigaDOS executable (`HUNK_HEADER`).
const HUNK_MAGIC: [u8; 4] = [0x00, 0x00, 0x03, 0xF3];

// Hunk block ids, as they appear on disk (see `dos/doshunks.h`).
const HUNK_NAME: u32 = 0x3E8;
const HUNK_CODE: u32 = 0x3E9;
const HUNK_DATA: u32 = 0x3EA;
const HUNK_BSS: u32 = 0x3EB;
const HUNK_RELOC32: u32 = 0x3EC;
const HUNK_RELOC16: u32 = 0x3ED;
const HUNK_RELOC8: u32 = 0x3EE;
const HUNK_SYMBOL: u32 = 0x3F0;
const HUNK_DEBUG: u32 = 0x3F1;
const HUNK_END: u32 = 0x3F2;
const HUNK_HEADER: u32 = 0x3F3;
const HUNK_OVERLAY: u32 = 0x3F5;
const HUNK_DREL32: u32 = 0x3F7;
const HUNK_DREL16: u32 = 0x3F8;
const HUNK_DREL8: u32 = 0x3F9;
const HUNK_RELOC32SHORT: u32 = 0x3FC;
const HUNK_RELRELOC32: u32 = 0x3FD;
const HUNK_ABSRELOC16: u32 = 0x3FE;

/// The top two bits of a hunk size (and of a hunk id) select the memory type to
/// load into; only the low 30 bits are the value itself.
const MEM_MASK: u32 = 0x3FFF_FFFF;

/// Does `path` hold an executable AmigaDOS would actually be able to load?
///
/// The `HUNK_HEADER` magic alone says very little — old BBS packs are full of
/// files that start with it but are truncated or otherwise mangled, and picking
/// one of those over the real demo next to it means booting into nothing. So
/// walk the whole hunk stream the way `LoadSeg()` does and reject anything that
/// doesn't hold together. Only longwords are read, data is seeked over, so this
/// stays cheap even for multi-megabyte executables.
fn is_amiga_exe(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return false;
    };
    let mut reader = HunkReader {
        inner: BufReader::new(file),
        pos: 0,
        len,
    };
    parse_exe(&mut reader).is_some()
}

/// Cursor over a hunk file that never reads (or seeks) past the end, so a
/// truncated file fails instead of quietly running out of blocks.
struct HunkReader<R> {
    inner: R,
    pos: u64,
    len: u64,
}

impl<R: Read + Seek> HunkReader<R> {
    /// Reserve the next `count` bytes, failing if they aren't all in the file.
    fn take(&mut self, count: u64) -> Option<()> {
        self.pos = self.pos.checked_add(count).filter(|p| *p <= self.len)?;
        Some(())
    }

    fn long(&mut self) -> Option<u32> {
        let mut buf = [0u8; 4];
        self.take(4)?;
        self.inner.read_exact(&mut buf).ok()?;
        Some(u32::from_be_bytes(buf))
    }

    fn word(&mut self) -> Option<u16> {
        let mut buf = [0u8; 2];
        self.take(2)?;
        self.inner.read_exact(&mut buf).ok()?;
        Some(u16::from_be_bytes(buf))
    }

    fn skip(&mut self, count: u64) -> Option<()> {
        self.take(count)?;
        self.inner.seek(SeekFrom::Start(self.pos)).ok()?;
        Some(())
    }
}

/// A relocation table of `(count, hunk, count * offset)` groups, ended by a zero
/// count. Offsets are longwords in the 32 bit hunks and words in the `DREL`
/// ones, where the whole table is padded out to a longword.
fn skip_relocs<R: Read + Seek>(
    reader: &mut HunkReader<R>,
    hunk_count: u32,
    short: bool,
) -> Option<()> {
    let mut words = 0u64;
    loop {
        let count = if short {
            u64::from(reader.word()?)
        } else {
            u64::from(reader.long()?)
        };
        words += 1;
        if count == 0 {
            break;
        }
        let hunk = if short {
            u32::from(reader.word()?)
        } else {
            reader.long()?
        };
        if hunk >= hunk_count {
            return None;
        }
        words += 1 + count;
        reader.skip(count * if short { 2 } else { 4 })?;
    }
    if short && words % 2 == 1 {
        reader.skip(2)?;
    }
    Some(())
}

fn parse_exe<R: Read + Seek>(reader: &mut HunkReader<R>) -> Option<()> {
    if reader.long()? != HUNK_HEADER {
        return None;
    }
    // Resident library names: longword-counted strings, ended by a zero length.
    loop {
        let count = reader.long()?;
        if count == 0 {
            break;
        }
        reader.skip(u64::from(count) * 4)?;
    }

    let hunk_count = reader.long()?;
    let first = reader.long()?;
    let last = reader.long()?;
    if first > last || last >= hunk_count {
        return None;
    }
    let mut sizes = Vec::new();
    for _ in first..=last {
        let size = reader.long()?;
        // Memory type `11` means an explicit attribute longword follows.
        if size >> 30 == 3 {
            reader.long()?;
        }
        sizes.push(u64::from(size & MEM_MASK));
    }

    for size in sizes {
        // Every hunk starts with the block holding its contents, and the header
        // already reserved the memory for it. A block may ask for less than was
        // reserved — crunchers do that to get scratch space — but never more.
        match reader.long()? & MEM_MASK {
            // An overlaid executable stops here and continues with an overlay
            // table describing hunks loaded on demand; the header is as far as
            // this walk usefully goes.
            HUNK_OVERLAY => return Some(()),
            HUNK_CODE | HUNK_DATA => {
                let longs = u64::from(reader.long()?);
                if longs > size {
                    return None;
                }
                reader.skip(longs * 4)?;
            }
            HUNK_BSS => {
                if u64::from(reader.long()?) > size {
                    return None;
                }
            }
            _ => return None,
        }
        // Relocations and debug information, up to the hunk's HUNK_END.
        loop {
            match reader.long()? & MEM_MASK {
                HUNK_END => break,
                HUNK_RELOC32 | HUNK_RELOC16 | HUNK_RELOC8 | HUNK_RELRELOC32 | HUNK_ABSRELOC16 => {
                    skip_relocs(reader, hunk_count, false)?;
                }
                HUNK_RELOC32SHORT | HUNK_DREL32 | HUNK_DREL16 | HUNK_DREL8 => {
                    skip_relocs(reader, hunk_count, true)?;
                }
                HUNK_SYMBOL => loop {
                    let count = reader.long()?;
                    if count == 0 {
                        break;
                    }
                    // Name, then the symbol's value.
                    reader.skip(u64::from(count) * 4 + 4)?;
                },
                HUNK_NAME | HUNK_DEBUG => {
                    let count = reader.long()?;
                    reader.skip(u64::from(count) * 4)?;
                }
                HUNK_OVERLAY => return Some(()),
                _ => return None,
            }
        }
    }
    // Trailing data past the last hunk is fine — demos do append their own.
    Some(())
}

#[derive(Default)]
pub struct AmigaSystem {
    aga: bool,
    xmem: bool,
    fast: bool,
    fast_load: bool,
    silent_drive: bool,
}

impl AmigaSystem {
    pub fn new(args: &Args) -> Self {
        Self {
            aga: args.aga,
            xmem: args.xmem,
            fast: args.fast,
            fast_load: args.fast_load,
            silent_drive: args.silent_drive,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum Cpu {
    M68000,
    M68020,
    M68030,
    M68040,
    M68060,
}

impl From<Cpu> for String {
    fn from(value: Cpu) -> Self {
        (match value {
            Cpu::M68000 => "68000",
            Cpu::M68020 => "68020",
            Cpu::M68030 => "68030",
            Cpu::M68040 => "68040",
            Cpu::M68060 => "68060",
        })
        .into()
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum Machine {
    A500OLD,
    A500,
    A1200,
    A4000,
}

impl From<Machine> for String {
    fn from(value: Machine) -> Self {
        (match value {
            Machine::A500OLD => "A500OG",
            Machine::A500 => "A500",
            Machine::A1200 => "A1200",
            Machine::A4000 => "A4040",
        })
        .into()
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kickstart {
    V12,
    V13,
    V20,
    V31A1200,
    V31A4000,
}

impl From<Kickstart> for String {
    fn from(value: Kickstart) -> Self {
        (match value {
            Kickstart::V12 => "kick33180.A500",
            Kickstart::V13 => "kick34005.A500",
            Kickstart::V20 => "kick37175.A500",
            Kickstart::V31A1200 => "kick40068.A1200",
            Kickstart::V31A4000 => "kick40068.A4000",
        })
        .into()
    }
}

impl WorkFile {
    fn set_fast_mem(&mut self, mb: usize) {
        let mbs = mb.to_string();
        self.set_meta("puae_fastmem_size", &mbs);
        self.set_meta("puae_z3mem_size", &mbs);
        self.set_meta("amiberry_z3mem_size", &mbs);
    }

    fn set_z3_mem(&mut self, mb: usize) {
        let mbs = mb.to_string();
        self.set_meta("puae_z3mem_size", &mbs);
        self.set_meta("amiberry_z3mem_size", &mbs);
    }

    fn set_cpu(&mut self, cpu: Cpu) {
        self.set_meta("puae_cpu_model", cpu);
        self.set_meta("amiberry_cpu_model", cpu);
    }
    fn set_machine(&mut self, machine: Machine) {
        self.set_meta("puae_model", machine);
        self.set_meta("amiberry_model", machine);
    }

    fn set_kickstart(&mut self, kickstart: Kickstart) {
        self.set_meta("puae_kickstart", kickstart);
        self.set_meta("amiberry_kickstart", kickstart);
    }

    fn set_fast(&mut self) {
        self.set_machine(Machine::A1200);
        self.set_cpu(Cpu::M68060);
        self.set_fast_mem(8);
        self.set_z3_mem(128);
        self.set_meta("amiberry_jit", "enabled");
        self.set_meta("amiberry_cpu_speed", "max");
        self.set_meta("puae_fpu_model", "68882");
    }

    fn is_aga(&self) -> bool {
        let model = self.get_meta("puae_model", "");
        model == "A1200" || model == "A4000"
    }
}

fn handle_exe(wf: &mut WorkFile, copy_all: bool) -> Result<()> {
    debug!("FMT: Amiga exe: {wf:?}");
    if std::fs::metadata(&wf)?.len() > 850 * 1024 {
        wf.set_machine(Machine::A1200);
        //wf.set_meta("puae_model", "A1200");
    }

    let target_dir = WorkFile::new_dir()?;
    let s_dir = target_dir.join("s");
    fs::create_dir(&s_dir)?;
    let c_dir = target_dir.join("c");
    fs::create_dir(&c_dir)?;
    fs::copy(system_dir().join("c").join("echo"), c_dir.join("echo"))?;
    let mut text: String = "".into();
    if wf.is_aga() {
        //let model = wf.get_meta("puae_model", "");
        //if model == "A1200" || model == "A4000" {
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
    wf.set_meta("puae_use_whdload", "disabled");

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

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("puae_model", "A500"),
            ("puae_crop", "smaller"),
            ("amiberry_crop_overscan", "disabled"),
            ("puae_horizontal_pos", "-5"),
            ("amiberry_video_vresolution", "auto"),
            ("puae_mapper_mouse_toggle", "---"),
        ]
        .into()
    }

    fn can_load(&self, path: &Path) -> bool {
        if has_any_extension(path, &["dms", "adf", "ips"]) {
            return true;
        }
        if has_extension(path, "cus") {
            return false;
        }
        let data = read_header(path, 4).unwrap_or_default();
        data.starts_with(&HUNK_MAGIC)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut exes = vec![];
        // Files that claim to be executables but don't parse. Kept as a last
        // resort so a release that only ships a mangled exe behaves as before.
        let mut broken = vec![];

        let aga = file.get_meta("platform", "").contains("AGA");

        if file.get_meta("puae_model", "") == "date" {
            file.set_meta("puae_model", "A500");
            if let Ok(year) = file.get_meta("year", "").parse::<u32>() {
                if year < 1990 {
                    file.set_kickstart(Kickstart::V12);
                    //file.set_meta("puae_kickstart", "kick33180.A500");
                } else if aga || self.aga {
                    file.set_machine(Machine::A4000);
                    // file.set_meta("puae_model", "A1200");
                    // file.set_meta("amiberry_model", "A1200");
                    // if year >= 1993 {
                    //     file.set_meta("puae_model", "A1200");
                    // }
                    if year >= 1995 {
                        file.set_cpu(Cpu::M68040);
                        //file.set_meta("puae_cpu_model", "68030");
                    }
                    if year >= 1997 {
                        file.set_fast();
                        // file.set_meta("puae_fastmem_size", "8");
                        // file.set_meta("puae_z3mem_size", "128");
                        // file.set_meta("puae_fpu_model", "68882");
                        // file.set_meta("amiberry_model", "A4040");
                        // file.set_meta("amiberry_cpu_model", "68040");
                        // file.set_meta("amiberry_z3mem_size", "128");
                        // file.set_meta("amiberry_jit", "enabled");
                        // file.set_meta("amiberry_cpu_speed", "max");
                        // file.set_meta("amiberry_kickstart", "kick40068.A4000");
                    }
                }
            }
        } else {
            if aga || self.aga {
                file.set_machine(Machine::A1200);
                //file.set_meta("puae_model", "A1200");
                //file.set_meta("amiberry_model", "A1200");
            }
        }
        if self.fast_load {
            file.set_meta("puae_floppy_speed", "0");
        }
        if self.xmem {
            file.set_fast_mem(8);
            file.set_z3_mem(128);
            // file.set_meta("puae_fastmem_size", "8");
            // file.set_meta("puae_z3mem_size", "128");
            // file.set_meta("amiberry_z3mem_size", "128");
        }
        if self.fast {
            file.set_fast();
            // file.set_meta("amiberry_cpu_model", "68040");
            // file.set_meta("amiberry_z3mem_size", "128");
            // file.set_meta("amiberry_jit", "enabled");
            // file.set_meta("amiberry_cpu_speed", "max");
            // file.set_meta("amiberry_kickstart", "kick40068.A4000");
            // file.set_meta("puae_model", "A1200");
            // file.set_meta("puae_fpu_model", "68882");
        }
        if self.silent_drive {
            file.set_meta("puae_floppy_sound", "100");
        }

        let copy_all = !file.is_file();

        let mut is_dir = false;
        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            if ext == "cus" || ext == "fp" {
                return Ok(()); // Custom music looks like exe
            }
            if ["adf", "dms"].contains(&ext) {
                images.push(path.to_owned());
            } else if ext == "slave" {
                file.set_meta("puae_model", "A1200");
                file.set_meta("puae_use_whdload", "enabled");
                is_dir = true;
            } else if header.starts_with(&HUNK_MAGIC) {
                if is_amiga_exe(path) {
                    exes.push(path.to_owned());
                } else {
                    debug!("Broken Amiga exe: {path:?}");
                    broken.push(path.to_owned());
                }
            } else if path.ends_with("s/startup-sequence") {
                // Auto-booting
                info!("Auto-booting");
                file.set_meta("puae_use_whdload", "disabled");
                if let Some(p) = path.parent()
                    && let Some(p) = p.parent()
                {
                    file.path = p.into();
                }
                is_dir = true;
            }
            Ok(())
        })?;

        if self.xmem {
            file.set_fast_mem(8);
            file.set_meta("puae_chipmem_size", "4");
        }

        if file.get_meta("platform", "").contains("AGA") {
            file.set_machine(Machine::A1200);
            //file.set_meta("puae_model", "A1200");
        }
        if file.has_tag("amos") {
            info!("AMOS DEMO");
            file.set_fast();
            file.set_fast_mem(8);
            file.set_z3_mem(8);
            // file.set_meta("puae_cpu_model", "68030");
            // file.set_meta("puae_z3mem_size", "128");
            // file.set_meta("puae_fastmem_size", "8");
            file.set_meta("puae_chipmem_size", "4");
            file.make_temp()?;
            let l_dir = file.temp_dir().unwrap().join("libs");
            fs::create_dir(&l_dir)?;
            fs::copy(
                system_dir().join("libs").join("mathtrans.library"),
                l_dir.join("mathtrans.library"),
            )?;
        }

        if is_dir {
            return Ok(true);
        }

        if !images.is_empty() {
            collect_disk_images(file, &mut images)?;
        } else if let Some(exe) = exes.first().or_else(|| broken.first()) {
            file.path = exe.clone();
            handle_exe(file, copy_all)?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let meta = path.get_all_meta();
        let core_name = if use_amiberry(path) {
            //puae_to_amiberry(&mut meta);
            CORE_NAME_AMIBERRY
        } else {
            CORE_NAME_UAE
        };
        debug!("Starting {core_name} with meta {meta:?}");
        let core = libloader::get_libretro(core_name).context("Could not load core")?;
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            meta,
            false,
        )?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Parse a hunk file written as a list of longwords.
    fn parses(longs: &[u32]) -> bool {
        let data: Vec<u8> = longs.iter().flat_map(|l| l.to_be_bytes()).collect();
        let len = data.len() as u64;
        parse_exe(&mut HunkReader {
            inner: Cursor::new(data),
            pos: 0,
            len,
        })
        .is_some()
    }

    /// One code hunk of two longwords, the smallest thing LoadSeg() accepts.
    const MINIMAL: [u32; 11] = [
        HUNK_HEADER,
        0,
        1,
        0,
        0,
        2,
        HUNK_CODE,
        2,
        0x4E71,
        0x4E75,
        HUNK_END,
    ];

    #[test]
    fn accepts_minimal_exe() {
        assert!(parses(&MINIMAL));
    }

    #[test]
    fn accepts_trailing_data() {
        let mut file = MINIMAL.to_vec();
        file.extend([0xDEAD, 0xBEEF]);
        assert!(parses(&file));
    }

    #[test]
    fn accepts_hunk_reserving_more_than_it_loads() {
        let mut file = MINIMAL;
        file[5] = 0x1000;
        assert!(parses(&file));
    }

    #[test]
    fn accepts_relocs_and_symbols() {
        assert!(parses(&[
            HUNK_HEADER,
            0,
            2,
            0,
            1,
            2,
            1,
            HUNK_CODE,
            2,
            0x4E71_4E71,
            0x4E75_0000,
            HUNK_RELOC32,
            1,
            1,
            0,
            0,
            HUNK_SYMBOL,
            1,
            0x6D61,
            0,
            0,
            HUNK_END,
            HUNK_BSS,
            1,
            HUNK_END,
        ]));
    }

    #[test]
    fn rejects_wrong_magic() {
        let mut file = MINIMAL;
        file[0] = HUNK_CODE;
        assert!(!parses(&file));
    }

    #[test]
    fn rejects_truncated_hunk() {
        assert!(!parses(&MINIMAL[..MINIMAL.len() - 2]));
    }

    #[test]
    fn rejects_missing_hunk_end() {
        assert!(!parses(&MINIMAL[..MINIMAL.len() - 1]));
    }

    #[test]
    fn rejects_block_bigger_than_reserved() {
        let mut file = MINIMAL;
        file[5] = 1;
        assert!(!parses(&file));
    }

    #[test]
    fn rejects_unknown_block() {
        let mut file = MINIMAL;
        file[6] = 0x1234;
        assert!(!parses(&file));
    }

    #[test]
    fn rejects_reloc_to_missing_hunk() {
        assert!(!parses(&[
            HUNK_HEADER,
            0,
            1,
            0,
            0,
            2,
            HUNK_CODE,
            2,
            0x4E71_4E71,
            0x4E75_0000,
            HUNK_RELOC32,
            1,
            1,
            0,
            0,
            HUNK_END,
        ]));
    }

    fn amiberry(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        let mut meta: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        puae_to_amiberry(&mut meta);
        meta
    }

    #[test]
    fn renames_puae_options() {
        let meta = amiberry(&[("puae_model", "A1200"), ("puae_cpu_model", "68030")]);
        assert_eq!(meta.get("amiberry_model").unwrap(), "A1200");
        assert_eq!(meta.get("amiberry_cpu_model").unwrap(), "68030");
    }

    #[test]
    fn drops_options_without_an_amiberry_equivalent() {
        let meta = amiberry(&[("puae_fpu_model", "68882"), ("puae_chipmem_size", "4")]);
        assert!(!meta.keys().any(|k| k.starts_with("amiberry_")));
    }

    #[test]
    fn folds_fastmem_into_z3mem_only_when_z3_is_unset() {
        assert_eq!(
            amiberry(&[("puae_fastmem_size", "8")])
                .get("amiberry_z3mem_size")
                .unwrap(),
            "8"
        );
        assert_eq!(
            amiberry(&[("puae_fastmem_size", "8"), ("puae_z3mem_size", "128")])
                .get("amiberry_z3mem_size")
                .unwrap(),
            "128"
        );
    }

    #[test]
    fn unthrottles_accelerated_cpus_only() {
        assert_eq!(
            amiberry(&[("puae_cpu_model", "68030")])
                .get("amiberry_cpu_speed")
                .unwrap(),
            "max"
        );
        assert!(!amiberry(&[("puae_model", "A500")]).contains_key("amiberry_cpu_speed"));
    }

    #[test]
    fn keeps_hand_written_amiberry_options() {
        let meta = amiberry(&[("puae_model", "A1200"), ("amiberry_model", "A4040")]);
        assert_eq!(meta.get("amiberry_model").unwrap(), "A4040");
    }

    #[test]
    fn rejects_missing_hunk_block() {
        let mut file = MINIMAL.to_vec();
        // Two hunks announced, only one present.
        file[2] = 2;
        file[4] = 1;
        file.insert(6, 1);
        assert!(!parses(&file));
    }
}
