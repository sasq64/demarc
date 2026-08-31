use anyhow::{Context, Result};
use std::{
    collections::HashMap,
    fs,
    io::{BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use tracing::{debug, info};

use crate::utils::{copy_dir_all, has_any_extension, has_extension, read_header};

use crate::{
    Args,
    backend::Backend,
    libloader,
    newsys::{collect_disk_images, walk_dir},
    retro_emu::RetroCoreThreaded,
    system_dir,
    workfile::WorkFile,
};

use super::System;

const CORE_NAME_UAE: &str = "puae";
/// Amiberry's libretro port. The libretro buildbot does not ship it, so it is
/// downloaded from its own release instead (see `ALT_SOURCES`) — or taken from
/// `$DEMARC_CORE_DIR` when a local build is being tested (see AMIBERRY.md).
const CORE_NAME_AMIBERRY: &str = "amiberry";

/// The Amiga corner of `system_dir()`: the Kickstart ROMs and the WHDLoad
/// assets, and the directory both Amiga cores are handed as their libretro
/// system (and save) directory.
///
/// It is a subdirectory rather than `system/` itself because amiberry's startup
/// ROM scan walks the directory it is given *recursively*, opens every file in
/// it, and probes it for an Amiga ROM by content — and the probe treats anything
/// carrying an archive signature as an archive, whatever the file is called.
/// `system/` is shared by every core, so that scan used to reach vice's,
/// musix's, PCem's and Ruffle's data as well. Two things went wrong:
///
/// * PCem's AMI BIOS images (`system/pcem/roms/430vx/55xwuq0e.bin`) carry
///   `-lh5-` at offset 2, because that is how AMI packs its modules — and
///   amiberry's LHA decoder then overruns a stack array unpacking one. The
///   process died in `__stack_chk_fail` inside `lha_make_table()` before
///   `retro_load_game` returned, taking demarc with it. Nothing on our side can
///   catch an `abort()` in a core, so the fix has to be to not show it the file.
/// * The scan CRC32s and SHA1s every one of those ~1000 files, which was the
///   ~2 s per demo start recorded in docs/AMIBERRY.md.
///
/// puae does not rummage — it looks its files up by name — but it reads the
/// same Kickstarts, so it gets the same directory. Amiberry also writes here
/// (`amiberry.ini`, `Configurations/`, `Savegames/`, WHDLoad save-data …),
/// which is why it is a real directory in `system/` and not something assembled
/// per run.
fn amiga_system_dir() -> PathBuf {
    system_dir().join("amiga")
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

    // The block that ended the previous hunk, when that hunk ran into this
    // one's contents instead of a HUNK_END.
    let mut pending = None;
    for size in sizes {
        // LoadSeg() reads one block at a time and skips the ones that carry no
        // memory, wherever they turn up, so a hunk may open with debugger or
        // name blocks before the block holding its contents — `eph-fels.exe`
        // leads with a HUNK_DEBUG.
        let mut block = match pending.take() {
            Some(block) => block,
            None => reader.long()? & MEM_MASK,
        };
        while block == HUNK_NAME || block == HUNK_DEBUG {
            let count = reader.long()?;
            reader.skip(u64::from(count) * 4)?;
            block = reader.long()? & MEM_MASK;
        }
        // Every hunk then has the block holding its contents, and the header
        // already reserved the memory for it. A block may ask for less than was
        // reserved — crunchers do that to get scratch space — but never more.
        match block {
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
                // HUNK_END is what LoadSeg() waits for, not what it needs: a
                // hunk is just as finished once the next one's contents turn
                // up, and linkers do leave the terminator out — `dcs-klone.exe`
                // runs its hunks straight into each other.
                block @ (HUNK_CODE | HUNK_DATA | HUNK_BSS) => {
                    pending = Some(block);
                    break;
                }
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
    A500PLUS,
    A600,
    A1200,
    A4000,
}

impl From<Machine> for String {
    fn from(value: Machine) -> Self {
        (match value {
            Machine::A500OLD => "A500OG",
            Machine::A500 => "A500",
            Machine::A500PLUS => "A500+",
            Machine::A600 => "A600",
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
        self.set_meta("amiberry_fastmem_size", &mbs);
    }

    fn set_chip_mem(&mut self, mb: usize) {
        let mbs = mb.to_string();
        self.set_meta("puae_chipmem_size", &mbs);
        self.set_meta("amiberry_chipmem_size", &mbs);
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
        self.set_cpu(Cpu::M68030);
        self.set_fast_mem(8);
        self.set_z3_mem(128);
        self.set_meta("amiberry_jit", "enabled");
        self.set_meta("amiberry_cpu_speed", "max");
        self.set_meta("puae_fpu_model", "68882");
        self.set_meta("amiberry_fpu_model", "68882");
    }

    fn is_aga(&self) -> bool {
        let model = self.get_meta_or("puae_model", "");
        model == "A1200" || model == "A4000" || model == "A4040"
    }
}

/// The `C:Assign` lines the `assign` meta (`NAME=path;NAME=path`) asks for.
/// They have to run before the demo does, so they go at the top of whichever
/// startup-sequence ends up booting it — the generated one in [`handle_exe`],
/// or the release's own (see [`patch_startup_sequence`]).
fn assign_commands(wf: &WorkFile) -> String {
    let mut text = String::new();
    for assign in wf.get_meta_or("assign", "").split(';') {
        if let Some((key, val)) = assign.split_once('=') {
            text += format!("C:Assign {key}: {val}\n").as_str();
        }
    }
    text
}

/// The entry named `name` in `dir`, matched without regard to case the way
/// AmigaDOS would — a release ships `C/Assign` or `c/assign` as it pleases.
fn find_ignoring_case(dir: &Path, name: &str) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name))
        .map(|e| e.path())
}

/// Insert the `assign` meta's assigns at the top of a release's own
/// startup-sequence, which is what boots it when it ships one (see the
/// auto-booting branch in [`AmigaSystem::load`]) — nothing else gets a chance
/// to make them.
///
/// `startup` is the sequence found under `file`, which is copied to a temp
/// directory first: the release is usually read straight off the file system,
/// and the drive the emulator mounts has to be writeable to be patched.
fn patch_startup_sequence(file: &mut WorkFile, startup: &Path) -> Result<()> {
    let assigns = assign_commands(file);
    if assigns.is_empty() {
        return Ok(());
    }
    let relative = startup.strip_prefix(&file.path)?.to_owned();
    file.make_temp()?;
    let startup = file.path.join(relative);

    // The drive is the release directory itself, so C: holds whatever the
    // release put there — often nothing at all, and then every assign line
    // fails silently and the demo starts without them. Lend it the one command
    // out of the skeleton drive.
    let c_dir = find_ignoring_case(&file.path, "c");
    if c_dir
        .as_deref()
        .and_then(|dir| find_ignoring_case(dir, "assign"))
        .is_none()
    {
        let assign = system_dir().join("amihdd").join("C").join("Assign");
        let c_dir = c_dir.unwrap_or_else(|| file.path.join("C"));
        if assign.is_file() {
            fs::create_dir_all(&c_dir)?;
            fs::copy(&assign, c_dir.join("Assign"))?;
        } else {
            debug!("No Assign command to lend: {assign:?}");
        }
    }

    info!("Adding assigns to {startup:?}");
    // Amiga text is not necessarily UTF-8, so the sequence is kept as bytes.
    let mut text = assigns.into_bytes();
    text.extend(fs::read(&startup)?);
    fs::write(&startup, text)?;
    Ok(())
}

fn handle_exe(wf: &mut WorkFile, copy_all: bool) -> Result<()> {
    debug!("FMT: Amiga exe: {wf:?}");
    if std::fs::metadata(&wf)?.len() > 850 * 1024 && !wf.is_aga() {
        wf.set_machine(Machine::A1200);
    }

    let target_dir = WorkFile::new_dir()?;
    // `system/amihdd/` is the skeleton of the generated drive: C: with the
    // commands a startup-sequence may reach for (`echo`, `SetPatch`, ...) and
    // LIBS: with the system libraries that aren't in ROM. The rest of those
    // shipped on the Workbench disk, so a drive without a LIBS: has none of
    // them. A demo that opens one gets a NULL back and, since it has nothing to
    // draw with, closes what it did open and exits with a zero return code and
    // no message — `eph-fels` does exactly that when `lowlevel.library` (the
    // keyboard and joypad, which nearly every AGA demo reads) isn't there,
    // which looks from the outside like it never ran. The release's own copies
    // win: they are copied over these below.
    let mut text: String = "".into();
    if wf.is_aga() {
        copy_dir_all(system_dir().join("amihdd"), &target_dir)?;
        text += "C:SetPatch QUIET\n";
        text += "C:MakeDir RAM:T RAM:Clipboards RAM:ENV RAM:ENV/Sys\nC:Copy >NIL: ENVARC: RAM:ENV ALL NOREQ\n";
        text += "C:Assign >NIL: ENV: RAM:ENV\n";
    } else {
        copy_dir_all(system_dir().join("ami13"), &target_dir)?;
    }

    text += &assign_commands(wf);

    let s_dir = target_dir.join("s");
    fs::create_dir_all(&s_dir)?;
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
            ("amiberry_floppy_speed", "100"),
            ("puae_crop", "smaller"),
            ("amiberry_crop_overscan", "manual"),
            ("amiberry_crop_width", "348"),
            ("amiberry_crop_height", "268"),
            ("puae_horizontal_pos", "-5"),
            ("amiberry_video_vresolution", "auto"),
            //{ "amiberry_overscan", "Overscan; overscan|tv_narrow|tv_standard|tv_wide|broadcast|extreme|ultra|ultra_hv|ultra_csync" },
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

        let aga = file.get_meta_or("platform", "").contains("AGA");

        if file.get_meta_or("puae_model", "") == "date" {
            file.set_meta("puae_model", "A500");
            if let Ok(year) = file.get_meta_or("year", "").parse::<u32>() {
                if year < 1990 {
                    file.set_kickstart(Kickstart::V12);
                    //file.set_meta("puae_kickstart", "kick33180.A500");
                } else if aga || self.aga {
                    // file.set_meta("puae_model", "A1200");
                    // file.set_meta("amiberry_model", "A1200");
                    // if year >= 1993 {
                    //     file.set_meta("puae_model", "A1200");
                    // }
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
                    } else if year >= 1995 {
                        file.set_machine(Machine::A1200);
                        file.set_cpu(Cpu::M68030);
                        //file.set_meta("puae_cpu_model", "68030");
                    } else {
                        file.set_machine(Machine::A1200);
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
        // The release's own startup-sequence, when it has one, patched with the
        // assigns once the walk is done and `file` can be borrowed again.
        let mut startup_sequence = None;
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
                startup_sequence = Some(path.to_owned());
                is_dir = true;
            }
            Ok(())
        })?;

        // A release that ships a file actually named `*.exe` means that one to
        // be started. Failing that, the shallowest executable: the one meant to
        // be run sits at the top of the release, while the deeper directories
        // hold the parts it loads (an intro, a trackmo's chapters, a bonus).
        // Ties are broken by path so the pick is stable.
        exes.sort_by(|a, b| {
            has_extension(b, "exe")
                .cmp(&has_extension(a, "exe"))
                .then_with(|| a.components().count().cmp(&b.components().count()))
                .then_with(|| a.cmp(b))
        });

        if file.has_tag("requires-1mb-chipmem") {
            file.set_chip_mem(2);
        }
        if file.has_tag("requires-68040") || file.has_tag("requires-68060") {
            file.set_cpu(Cpu::M68040);
        }

        //file.set_machine(Machine::A1200);
        if self.xmem {
            file.set_fast_mem(8);
            file.set_meta("puae_chipmem_size", "4");
        }

        if file.get_meta_or("platform", "").contains("AGA") {
            //file.set_machine(Machine::A1200);
            //file.set_meta("puae_model", "A1200");
        }
        if file.is_aga() {
            file.set_meta("amiberry_jit", "enabled");
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
            // file.make_temp()?;
            // let l_dir = file.temp_dir().unwrap().join("libs");
            // fs::create_dir(&l_dir)?;
            // fs::copy(
            //     system_dir().join("libs").join("mathtrans.library"),
            //     l_dir.join("mathtrans.library"),
            // )?;
        }

        if file.get_meta_or("amiga_core", "").is_empty() {
            file.set_meta(
                "amiga_core",
                if file.is_aga() { "amiberry" } else { "puae" },
            );
        }

        if let Some(startup) = startup_sequence {
            patch_startup_sequence(file, &startup)?;
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
        let core_name = if path.get_meta_or("amiga_core", "").contains("puae") {
            CORE_NAME_UAE
        } else {
            CORE_NAME_AMIBERRY
        };
        debug!("Starting {core_name} with meta {meta:?}");
        let core = libloader::get_libretro(core_name).context("Could not load core")?;
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            &amiga_system_dir(),
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

    /// A hunk may open with blocks that carry no memory, which LoadSeg() skips
    /// wherever it meets them — `eph-fels.exe` puts a HUNK_DEBUG in front of its
    /// first HUNK_CODE, and rejecting it costs the real demo to a lesser file.
    #[test]
    fn accepts_debug_block_before_the_hunk_contents() {
        let mut file = MINIMAL[..6].to_vec();
        file.extend([HUNK_DEBUG, 2, 0xF00D, 0xF00D]);
        file.extend(&MINIMAL[6..]);
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

    /// HUNK_END is a terminator LoadSeg() waits for rather than one it needs,
    /// and linkers do leave it out — `dcs-klone.exe` runs its first hunk
    /// straight into the second, and is a whole demo to lose over a marker.
    #[test]
    fn accepts_hunk_running_into_the_next_without_an_end() {
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

    /// A release that boots itself keeps its own startup-sequence, so the
    /// assigns have to be inserted in front of it rather than replace it.
    #[test]
    fn inserts_assigns_before_an_existing_startup_sequence() {
        let dir = tempfile::Builder::new().tempdir().unwrap();
        fs::create_dir_all(dir.path().join("s")).unwrap();
        let startup = dir.path().join("s/startup-sequence");
        fs::write(&startup, "demo\n").unwrap();

        let mut file = WorkFile::new(dir.path());
        file.set_meta("assign", "data=dh0:stuff;musik=dh0:mod");
        patch_startup_sequence(&mut file, &startup).unwrap();

        // Patched in a temp copy, the release itself untouched.
        assert!(file.is_temporary());
        assert_eq!(fs::read_to_string(&startup).unwrap(), "demo\n");
        assert_eq!(
            fs::read_to_string(file.path.join("s/startup-sequence")).unwrap(),
            "C:Assign data: dh0:stuff\nC:Assign musik: dh0:mod\ndemo\n"
        );
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
