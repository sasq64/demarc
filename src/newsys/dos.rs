use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::{System, get_ext, walk_dir};
use crate::backend::Backend;
use crate::libloader;
use crate::retro_emu::RetroCoreThreaded;
use crate::system_dir;
use crate::utils::read_at;
use crate::workfile::WorkFile;

const CORE_NAME_PCEM: &str = "pcem";
const CORE_NAME_DOSBOX: &str = "dosbox_pure";

/// Meta key asking for the Watcom extender to be placed beside the program.
const META_DOS4GW: &str = "dos4gw";

/// What the extender is called once it is in place. DOS uppercases every name
/// it prints and most releases ship it this way, so the copy matches.
const DOS4GW_EXE: &str = "DOS4GW.EXE";

/// Where a copy of it is kept, under the system dir.
fn dos4gw_source() -> PathBuf {
    system_dir().join("dos").join("dos4gw.exe")
}

/// PC/DOS through PCem or DOSBox.
///
/// Two very different ways of running a PC, picked by what the release is:
///
/// - A PCem machine `.cfg` — the same file the desktop PCem writes into its
///   `configs/` directory and takes with `--config` — goes to PCem. It names
///   the machine, CPU, video and sound cards and the disc images to mount, so
///   it is the whole of the configuration; the core has no machine picker.
/// - A bare DOS program (`.exe`, `.com`, `.bat`) goes to DOSBox Pure, which
///   brings its own DOS and mounts the directory the program sits in as C:.
///   Nothing else is needed, which is what most DOS releases arrive as.
///
/// A `.exe` with a `PE` image behind its DOS stub is a Windows program and
/// none of this business — see [`super::windows`], which reads the same header
/// from the other side.
///
/// A DOS release is often missing the extender it was linked against, since
/// that came with the compiler rather than the demo. `dos4gw=true` on an entry
/// says so: the release is copied somewhere writable and a `DOS4GW.EXE` is put
/// beside the program — see [`place_extender`].
///
/// Neither core ships BIOS ROMs — DOSBox needs none, and PCem's are
/// copyrighted, so they must be placed under
/// `<system dir>/pcem/roms/<machine>/`; `docs/roms.txt` in the PCem tree lists
/// what each machine needs. Everything the machine writes — NVR, logs — goes
/// under `<save dir>/pcem/`.
pub struct DosSystem {}

/// Does this look like a PCem machine config?
///
/// `.cfg` is far too generic an extension to accept on its own — plenty of
/// systems drop one next to their content — so require the one key every PCem
/// machine config has and nothing else uses: a `model =` naming the machine.
fn is_pcem_config(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("model")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| !value.trim().is_empty())
    })
}

/// The largest a `.com` can be: DOS loads one into a single segment, below the
/// stack it puts at the top of it.
const MAX_COM_SIZE: u64 = 0xff00;

/// What an `MZ` file turns out to be once the stub is looked past.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ExeKind {
    /// Not an executable at all.
    None,
    /// DOS — a DOS extender (`LE`/`LX`) included, which is how half the demos
    /// of the era were built.
    Dos,
    /// A `PE` image behind the stub: Win32.
    Windows,
    /// `NE`: Windows 3.x or OS/2, which is not what any of this is for.
    Legacy,
}

/// Read what kind of program an `.exe` holds.
///
/// The check matters: the same extension and the same `MZ` header belong to
/// every Windows program ever built, and DOSBox can run none of those. What
/// sits at `e_lfanew` tells them apart — a second header there means the `MZ`
/// is only the stub in front of the real image. It is read here, beside the
/// DOS header it belongs to, and [`super::windows`] asks it the one question
/// from the other side.
///
/// The offset is not required to clear the DOS header. A size-optimised
/// release — which is most of the 64K Windows intros — overlaps the two, so
/// that the `PE` lands as early as 0x0c and the fields behind it double as the
/// rest of the DOS header. Only an offset landing on the `MZ` magic itself is
/// out of bounds.
pub(super) fn exe_kind(path: &Path) -> ExeKind {
    let Ok(size) = fs::metadata(path).map(|m| m.len()) else {
        return ExeKind::None;
    };
    let Ok(header) = read_at(path, 0, 0x40) else {
        return ExeKind::None;
    };
    if header.len() < 0x40 || !matches!(&header[..2], b"MZ" | b"ZM") {
        return ExeKind::None;
    }
    let lfanew = u64::from(u32::from_le_bytes(header[0x3c..0x40].try_into().unwrap()));
    // Plain DOS executables leave the field alone, so anything that isn't a
    // sane offset into the file is one of those.
    if lfanew < 2 || lfanew + 4 > size {
        return ExeKind::Dos;
    }
    match read_at(path, lfanew, 4).unwrap_or_default().as_slice() {
        b"PE\0\0" => ExeKind::Windows,
        [b'N', b'E', ..] => ExeKind::Legacy,
        _ => ExeKind::Dos,
    }
}

/// Does this look like something DOS would run?
///
/// `.exe` is the only one of the three with anything to check — see
/// [`exe_kind`].
fn is_dos_program(path: &Path) -> bool {
    let Ok(size) = fs::metadata(path).map(|m| m.len()) else {
        return false;
    };
    match get_ext(path).as_str() {
        "exe" => exe_kind(path) == ExeKind::Dos,
        // A `.com` is a raw memory image with no header to recognise, so its
        // size is all there is to go on.
        "com" => size > 0 && size <= MAX_COM_SIZE,
        // A batch file is text, and an empty one starts nothing.
        "bat" => size > 0 && fs::read(path).is_ok_and(|b| std::str::from_utf8(&b).is_ok()),
        _ => false,
    }
}

/// How much we want to start a given program, biggest first.
///
/// A release is usually a directory holding one program worth running and
/// several that aren't — an installer, a setup tool, a viewer for the .NFO —
/// and the walk reaches them in whatever order the filesystem gives. So rank
/// them: the file named after the release is what the release is, and anything
/// called INSTALL or SETUP is the one thing we know we don't want.
///
/// A `.bat` loses to any `.exe` beside it. It reads like the author saying
/// "start here", but a release that ships one is as often using it to print the
/// .NFO or set a variable before handing over — whereas the `.exe` beside it is
/// the demo itself, and starts the same either way.
///
/// Between two programs that are otherwise equal, the one with a plain 8.3 name
/// wins — see [`is_simple_name`]. A DOS release could not have been built around
/// a name DOS cannot type, so a long or non-ASCII one was given to the file
/// afterwards, by whoever packed or re-packed it: an unpacker, a scene archive,
/// or a "read me first ⭐.exe" wrapper. The program the release actually is
/// still carries the name it was linked as.
fn launch_rank(path: &Path, release: &str) -> i32 {
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();

    let mut rank = match get_ext(path).as_str() {
        "bat" => 10,
        "exe" => 20,
        _ => 0,
    };
    if !release.is_empty() && stem == release {
        rank += 10;
    }
    if ["install", "setup", "config", "uninstal", "readme"].contains(&stem.as_str()) {
        rank -= 20;
    }
    // An extender is loaded by the program that was linked against it, never
    // started by hand: on its own it puts up a usage banner and quits. It is
    // worth the same penalty, since a release shipping one holds the program
    // that needs it too.
    if EXTENDERS.contains(&stem.as_str()) {
        rank -= 20;
    }
    // Small enough to break a tie between two programs without reaching across
    // the ranks above it: an INSTALL.EXE stays below a demo whatever the demo
    // is called.
    if is_simple_name(path) {
        rank += 5;
    }
    rank
}

/// Is this a name DOS itself could hold — 8.3, ASCII, no spaces?
///
/// Both halves have to be there and within length, and every character has to
/// be one DOS accepts in a name: letters, digits, and the punctuation it left
/// alone. Case is not part of it, since the same release arrives uppercased,
/// lowercased or mixed depending on what unpacked it.
fn is_simple_name(path: &Path) -> bool {
    // Not `to_string_lossy`: a name that is not valid UTF-8 has bytes in it we
    // would replace with U+FFFD and then have to reject anyway.
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    // `rsplit_once` so that the last dot is the one that separates them, which
    // also means a second dot lands in the stem and is rejected there: DOS has
    // exactly one, and "readme.txt.exe" is not a name it could have held.
    (1..=8).contains(&stem.len())
        && (1..=3).contains(&ext.len())
        && [stem, ext]
            .iter()
            .all(|part| part.chars().all(|c| DOS_NAME_CHARS.contains(c)))
}

/// What may appear in a DOS file name: the alphanumerics, and the punctuation
/// left over once the characters DOS uses itself are taken out — the path
/// separators, the wildcards, and the command-line delimiters.
const DOS_NAME_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'()-@^_`{}~";

/// The DOS extenders and DPMI hosts a release ships beside its program.
const EXTENDERS: &[&str] = &[
    "dos4gw", "dos4g", "dos32a", "cwsdpmi", "cwsdpr0", "pmodew", "wdosx", "dpmiload", "dpmi16bi",
];

/// Put the Watcom extender beside the program we are about to start.
///
/// A program linked against DOS/4GW loads `DOS4GW.EXE` at startup, from the
/// current directory or the PATH, and plenty of releases were packed without
/// it — it came with the compiler, and every machine of the era had one lying
/// around. DOSBox starts in the directory it mounted as C:, which is the one
/// the program sits in, so that is where the copy goes.
fn place_extender(file: &WorkFile, source: &Path) -> Result<()> {
    if !source.is_file() {
        warn!("No extender to copy at {source:?}");
        return Ok(());
    }
    let dir = file.path.parent().context("Program has no directory")?;
    // A release that ships its own extender has already answered the question,
    // and DOS names come back from an archive in every case there is.
    let has_one = fs::read_dir(dir)?.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(DOS4GW_EXE)
    });
    if has_one {
        info!("{dir:?} already holds an extender");
        return Ok(());
    }
    let target = dir.join(DOS4GW_EXE);
    fs::copy(source, &target)
        .with_context(|| format!("Could not copy {source:?} to {target:?}"))?;
    info!("Placed {target:?}");
    Ok(())
}

impl DosSystem {
    /// Which of the files in a release is the one to start.
    ///
    /// A machine config describes the whole machine, so it wins outright;
    /// otherwise the programs are ranked against each other and the best one
    /// taken — see [`launch_rank`]. `dir` names the release, which is how a
    /// program named after it is recognised, and may equally be a single file.
    fn pick_target(&self, dir: &Path) -> Result<Option<PathBuf>> {
        let release = dir
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let mut config = None;
        let mut best: Option<(i32, PathBuf)> = None;
        walk_dir(dir, 0, |path, ext, _| {
            if !self.can_load(path) {
                return Ok(());
            }
            if ext == "cfg" {
                config.get_or_insert_with(|| path.to_owned());
            } else {
                let rank = launch_rank(path, &release);
                if best.as_ref().is_none_or(|(top, _)| rank > *top) {
                    best = Some((rank, path.to_owned()));
                }
            }
            Ok(())
        })?;
        Ok(config.or(best.map(|b| b.1)))
    }
}

impl System for DosSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["cfg", "exe", "com", "bat"]
    }

    fn can_load(&self, path: &Path) -> bool {
        if !self.handles_ext(path) {
            return false;
        }
        if get_ext(path) == "cfg" {
            is_pcem_config(path)
        } else {
            is_dos_program(path)
        }
    }

    /// Narrow a release down to the program to start, and give it the
    /// extender if the release was marked as needing one.
    ///
    /// The default [`System::load`] does the narrowing; what it can't do is
    /// the order this needs. Copying the release into a temp dir moves every
    /// path inside it, so which file to start has to be settled first and
    /// followed across the copy afterwards.
    /// The default walks for the first file it can load, in whatever order the
    /// filesystem hands them over — which for a release directory holding
    /// several programs is not a choice at all. See [`DosSystem::pick_target`].
    fn get_first_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        self.pick_target(dir)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let Some(target) = self.pick_target(file)? else {
            return Ok(false);
        };

        debug!("FILE: {file:?}");

        if file.has_tag("needs-mmx") {
            file.set_meta("dosbox_pure_cpu_type", "pentium_mmx");
        }

        // A machine config brings its own DOS on its own disc images, so the
        // extender is only ever a question for a program run under DOSBox.
        let use_4gw = get_ext(&target) != "cfg"
            && (file.has_tag(META_DOS4GW) || !file.get_meta_or(META_DOS4GW, "").is_empty());

        if use_4gw {
            debug!("Needs DOS4GW");
            file.make_temp()?;
        }
        let Some(target) = self.pick_target(file)? else {
            return Ok(false);
        };
        file.path = target.clone();
        if use_4gw {
            place_extender(file, &dos4gw_source())?;
        }
        Ok(true)
    }

    /// DOSBox Pure reports the raw framebuffer ratio (a 320x200 mode comes out
    /// as 1.6, i.e. 16:10) unless aspect correction is on. With it on the core
    /// leaves the framebuffer alone and only reports the pixel-aspect-corrected
    /// display ratio, which is what a CRT showed and what our scaler wants.
    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("dosbox_pure_gus", "true"),
            ("dosbox_pure_cycles", "200000"),
            ("dosbox_pure_memory_size", "64"),
            ("dosbox_pure_aspect_correction", "true"),
        ]
        .into()
    }

    fn name(&self) -> &'static str {
        "MS/DOS"
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let core = libloader::get_libretro(core_for(&path.path)).context("Could not load core")?;
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            path.get_all_meta(),
            false,
        )?))
    }
}

/// Which core runs this file: PCem drives a machine config, DOSBox runs a
/// program on a DOS of its own.
fn core_for(path: &Path) -> &'static str {
    if get_ext(path) == "cfg" {
        CORE_NAME_PCEM
    } else {
        CORE_NAME_DOSBOX
    }
}

#[cfg(test)]
#[path = "tests/dos_tests.rs"]
mod tests;

/// Reading the emulated screen back as text.
///
/// A pixel hash would say the frame changed, not that the machine booted, and
/// it would go stale on any cosmetic change in PCem. Decoding the text instead
/// lets the test assert on what the BIOS actually printed.
#[cfg(test)]
#[path = "tests/dos_screen.rs"]
mod screen;
