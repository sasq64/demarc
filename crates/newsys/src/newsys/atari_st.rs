use anyhow::{Context, Result};
use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use crate::utils::{build_m3u, copy_dir_all, find_child, has_extension, sort_disks};

use crate::{
    SysOpts, backend::Backend, libloader, newsys::walk_dir, retro_emu, system_dir,
    workfile::WorkFile,
};

use super::System;

const CORE_NAME_HATARI: &str = "hatari";
const GEMDOS_MAGIC: [u8; 2] = [0x60, 0x1a];

/// Machines hatari's `--machine` accepts, the first being its default.
///
/// It is handed the value of `hatari_machinetype` as it stands, and rejects an
/// unknown one by abandoning the rest of its command line along with it — the
/// `--harddrive` that mounts C: included, which leaves a hard drive release
/// sitting on the TOS desktop with no drive to run from.
const MACHINE_TYPES: [&str; 4] = ["st", "ste", "tt", "falcon"];

#[derive(Default)]
pub struct AtariStSystem {
    falcon: bool,
}

impl AtariStSystem {
    pub fn new(args: &SysOpts) -> Self {
        Self {
            falcon: args.falcon,
        }
    }
}

/// Falcon defaults, each applied unless the release already sets it.
const FALCON_META: [(&str, &str); 6] = [
    ("hatari_dsp_type", "emu"),
    ("hatari_cpu_level", "3"),
    ("hatari_cpu_clock", "16"),
    ("hatari_fpu_type", "68882"),
    ("hatari_memory_size", "14336"),
    ("hatari_monitor_type", "rgb"),
];

const FALCON_TOS: &str = "tos404.img";

/// Does the program's header ask to be loaded into, or allocate from, TT-RAM?
fn wants_tt_ram(prg: &Path) -> bool {
    let mut header = [0u8; 26];
    fs::File::open(prg)
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut header))
        .is_ok_and(|_| header[25] & 0x06 != 0)
}

fn is_falcon_platform(file: &WorkFile) -> bool {
    let platform = file.get_meta_or("platform", "");
    !platform.is_empty()
        && platform
            .split(';')
            .all(|name| name.trim().eq_ignore_ascii_case("Atari Falcon"))
}

/// The core always boots `<system dir>/tos.img`, which is an ST TOS that the
/// Falcon refuses, so a Falcon gets a system dir of its own holding TOS 4.04
/// under that name, plus the `hatari/BOOT.ST` the hard drive boot needs.
fn falcon_system_dir() -> Result<PathBuf> {
    let hatari = system_dir().join("hatari");
    let dir = hatari.join("falcon");
    let files = [
        (hatari.join("tos").join(FALCON_TOS), dir.join("tos.img")),
        (hatari.join("BOOT.ST"), dir.join("hatari").join("BOOT.ST")),
    ];
    for (from, to) in files {
        let stale = match (from.metadata(), to.metadata()) {
            (Ok(f), Ok(t)) => f.len() != t.len(),
            _ => true,
        };
        if stale {
            fs::create_dir_all(to.parent().unwrap())?;
            fs::copy(&from, &to).with_context(|| format!("Falcon needs {from:?}"))?;
        }
    }
    Ok(dir)
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

/// How TOS names a program meant to be started. A release names what the user
/// runs and leaves its payload looking like data, so one of these beats a
/// GEMDOS executable called anything else, however big — `molz.tos` is a
/// 651-byte loader sitting next to the 652K part it loads.
const PROGRAM_EXTENSIONS: [&str; 4] = ["prg", "tos", "ttp", "app"];

/// Which of `exes` to boot.
///
/// `boot_file` decides it outright when the release carries one, named either
/// by file name or by path within the release (`DEMO/TLKTLK2.PRG`), matched
/// case insensitively. Otherwise it is a guess, in this order: named like a
/// program, nearest the top of the release, then the biggest of those. Size
/// alone picked a readme viewer over the demo beside it, and picked a payload
/// over the loader that feeds it — see docs/NOTES.md.
///
/// Paths are ranked relative to `root`, the release the walk found them in.
fn pick_program<'a>(exes: &'a [PathBuf], root: &Path, boot_file: &str) -> Option<&'a PathBuf> {
    let within = |p: &'a Path| p.strip_prefix(root).unwrap_or(p).to_owned();

    if !boot_file.is_empty() {
        // GEMDOS spells a path with backslashes and the host doesn't, so a
        // `boot_file` copied out of a release's own README still matches.
        let wanted = boot_file.replace('\\', "/");
        let named = exes.iter().find(|p| {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let path = within(p).to_string_lossy().replace('\\', "/");
            name.eq_ignore_ascii_case(&wanted) || path.eq_ignore_ascii_case(&wanted)
        });
        if let Some(named) = named {
            debug!("FMT: booting {named:?}, named by boot_file");
            return Some(named);
        }
        warn!("No program called {boot_file:?} in the release; picking one instead");
    }

    exes.iter().min_by_key(|p| {
        let named_like_program = PROGRAM_EXTENSIONS.iter().any(|e| has_extension(p, e));
        let size = p.metadata().map(|m| m.len()).unwrap_or(0);
        // `false` sorts before `true`, so each term is written to make the
        // wanted one the smaller: named like a program, then shallow, then big.
        (
            !named_like_program,
            within(p).components().count(),
            Reverse(size),
            // Two files alike in every way still have to rank in some order
            // that doesn't change from one walk to the next.
            p.as_path(),
        )
    })
}

/// Stage `prg` and the rest of its release as an Atari ST hard drive for
/// hatari, which mounts a host directory as GEMDOS drive C: and boots from it.
/// `base` is what we were pointed at, and bounds what goes on the drive.
/// Returns the path to hand the core and the temp directory holding it, which
/// the caller has to keep alive for as long as the drive is needed.
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
fn build_gemdos_drive(prg: &Path, base: &Path) -> Result<WorkFile> {
    let mut temp = WorkFile::new_dir()?;
    let drive = temp.join("harddrive");

    // The release is `base` and nothing outside it. Pointed straight at a
    // program file, the release *is* that one file: the directory it happens to
    // sit in belongs to whoever pointed us there — a downloads folder, a home
    // directory — and copying that wholesale is how the drive grows to
    // gigabytes and the copy never finishes.
    let mut in_auto = false;
    if base.is_dir() {
        let mut root = prg.parent().unwrap_or(base);
        // A program already in an `AUTO` folder is started by the folder above
        // it — as long as that folder is still part of the release.
        in_auto = root != base
            && root
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("auto"));
        if in_auto && let Some(parent) = root.parent() {
            root = parent;
        }
        copy_dir_all(root, &drive)?;
    } else {
        fs::create_dir_all(&drive)?;
    }

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

// TODO: args.fast_load => hatari_fast_floppy=enabled
impl System for AtariStSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_HATARI
    }

    fn name(&self) -> &'static str {
        "Atari ST"
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            //("hatari_forcerefresh", "2"),
            //("hatari_start_in_mouse_mode", "false"),
            ("hatari_fastboot", "enabled"),
            ("hatari_borders", "enabled"),
            //("hatari_led_status_display", "false"),
            //("hatari_video_crop_overscan", "false"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut exes = vec![];
        for (key, val) in self.default_meta() {
            file.set_meta(key, val);
        }

        if self.falcon || is_falcon_platform(file) {
            file.set_meta("hatari_machinetype", "falcon");
        }

        // What the release itself asks for, as against what is guessed for it
        // below: a hard drive load replaces a guess, but never a request.
        let machine_given = {
            let machine = file.get_meta_or("hatari_machinetype", "");
            !machine.is_empty() && machine != "date"
        } || file.has_tag("ste");
        let ram_given = file.has_meta("hatari_memory_size")
            || ["requires-4mb", "requires-2mb", "requires-1mb"]
                .iter()
                .any(|tag| file.has_tag(tag));

        // `date` is our own stand-in for "decide from the release's year", and
        // never a machine the core knows, so it has to be resolved here whether
        // the year says anything or not — see [`MACHINE_TYPES`] for what an
        // unresolved one costs.
        if file.get_meta_or("hatari_machinetype", "") == "date" {
            let year = file.get_meta_or("year", "").parse::<u32>().unwrap_or(0);
            info!("FMT: picking the machine from year {year}");
            if year > 1994 {
                file.set_meta("hatari_machinetype", "ste");
                file.set_meta("hatari_memory_size", "4096");
            } else {
                // Undated too: an STE release usually says so, while plenty of
                // ST ones break on an STE.
                file.set_meta("hatari_machinetype", "st");
            }
        }

        if file.has_tag("ste") {
            file.set_meta("hatari_machinetype", "ste");
        }

        // A machine the core can't parse takes everything after it down with
        // it, so nothing may reach it but the four it knows.
        let mut machine = file.get_meta_or("hatari_machinetype", "");
        if !machine.is_empty() && !MACHINE_TYPES.contains(&machine.as_str()) {
            warn!("Ignoring unknown hatari_machinetype {machine:?}");
            machine = MACHINE_TYPES[0].into();
            file.set_meta("hatari_machinetype", machine.as_str());
        }

        if machine == "falcon" {
            for (key, val) in FALCON_META {
                if !file.has_meta(key) {
                    file.set_meta(key, val);
                }
            }
        }
        if file.has_tag("requires-4mb") {
            file.set_meta("hatari_memory_size", "4096");
        } else if file.has_tag("requires-2mb") {
            file.set_meta("hatari_memory_size", "2048");
        } else if file.has_tag("requires-1mb") {
            file.set_meta("hatari_memory_size", "1024");
        }

        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            if ["msa", "st", "stx", "dim"].contains(&ext) {
                images.push(path.to_owned());
            } else if header[0..2] == GEMDOS_MAGIC {
                exes.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            if images.len() > 1 {
                sort_disks(&mut images);
                let m3u = build_m3u(&images, file)?;
                file.path = m3u;
            } else {
                file.path = images[0].clone();
            }
        } else if !exes.is_empty() {
            let prg = pick_program(&exes, &file.path, &file.get_meta_or("boot_file", ""))
                .expect("exes is not empty");
            info!("FMT: booting {prg:?} from a GEMDOS hard drive");

            // A release shipped to run off a hard drive is a late one: it
            // expects the STE and the memory that came with it, and a demo like
            // molz quietly drops back to the desktop on the 1MB ST that is the
            // core's default (docs/NOTES.md).
            if !machine_given {
                file.set_meta("hatari_machinetype", "ste");
            }
            if machine == "falcon" && wants_tt_ram(prg) && !file.has_meta("hatari_ttram_size") {
                file.set_meta("hatari_cpu_addr24", "disabled");
                file.set_meta("hatari_ttram_size", "64");
            }
            if !ram_given && machine != "falcon" {
                file.set_meta("hatari_memory_size", "4096");
            }

            let wf = build_gemdos_drive(prg, &file.path)?;
            file.path = wf.path;
            file.temp_dir = wf.temp_dir;
            return Ok(true);
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let core = libloader::get_libretro(self.core_name()).context("Could not load core")?;
        let system = if path.get_meta_or("hatari_machinetype", "") == "falcon" {
            falcon_system_dir()?
        } else {
            system_dir().to_path_buf()
        };
        retro_emu::create_core(&core, &system, Some(path), path.get_all_meta(), false)
    }
}

#[cfg(test)]
#[path = "tests/atari_st_tests.rs"]
mod tests;
