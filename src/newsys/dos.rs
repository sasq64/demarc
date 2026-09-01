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
    system_dir().join("pcem").join("dos4gw.exe")
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

/// Get the release into a directory we may write to, still pointing at
/// `target`.
///
/// What comes along is the whole of what we were given, not just the program:
/// a release directory holds the data files it opens, and DOSBox mounts that
/// directory as C:. Anything unpacked from an archive is in a temp dir
/// already and only needs the path narrowed.
fn make_writable(file: &mut WorkFile, target: &Path) -> Result<()> {
    if file.is_temporary() {
        file.path = target.to_owned();
        return Ok(());
    }
    let rel = if file.path.is_dir() {
        target
            .strip_prefix(&file.path)
            .context("Program is not inside the release")?
            .to_owned()
    } else {
        // A program pointed at directly. `make_temp` would copy that one file
        // and nothing else, so aim it at the directory the program sits in —
        // which is the release, as far as anything here can tell — and walk
        // back down to the program afterwards.
        let name = target.file_name().context("Program has no file name")?;
        let dir = target.parent().unwrap_or(Path::new(""));
        file.path = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir.to_owned()
        };
        PathBuf::from(name)
    };
    file.make_temp()?;
    file.path = file.path.join(rel);
    Ok(())
}

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

/// Is a meta value one of the ways of saying yes?
fn is_set(file: &WorkFile, key: &str) -> bool {
    matches!(
        file.get_meta_or(key, "").to_ascii_lowercase().as_str(),
        "true" | "1" | "yes"
    ) || file.has_tag("dos4gw")
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

        // A machine config brings its own DOS on its own disc images, so the
        // extender is only ever a question for a program run under DOSBox.
        if get_ext(&target) != "cfg" && is_set(file, META_DOS4GW) {
            debug!("Needs DOS4GW");
            make_writable(file, &target)?;
            place_extender(file, &dos4gw_source())?;
        } else {
            file.path = target;
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
            ("dosbox_pure_cycles", "150000"),
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
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use clap::Parser;

    use crate::Args;
    use crate::libretro::RETROK_RETURN;
    use crate::newsys::NewSys;
    use crate::system_dir;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        write_bytes(dir, name, body.as_bytes())
    }

    fn write_bytes(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    /// `.cfg` is a name half the world uses, so the sniff has to lean on the
    /// `model =` key rather than the extension.
    #[test]
    fn tells_a_machine_config_from_any_other_cfg() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};

        let pcem = write(
            dir.path(),
            "486.cfg",
            "model = ami486\ncpu = 0\nmem_size = 16384\ngfxcard = tgui9440\n",
        );
        assert!(sys.can_load(&pcem));

        // Section headers and spacing vary between hand-written configs.
        let spaced = write(dir.path(), "xt.cfg", "\n[Machine]\n  model=ibmxt\n");
        assert!(sys.can_load(&spaced));

        // Some other emulator's settings file that happens to end in .cfg.
        let other = write(dir.path(), "other.cfg", "fullscreen = 1\nscale = 2\n");
        assert!(!sys.can_load(&other));

        // A `model` key with nothing after it configures no machine.
        let empty = write(dir.path(), "empty.cfg", "model = \n");
        assert!(!sys.can_load(&empty));

        // A key that merely starts with "model" is not the model key.
        let lookalike = write(dir.path(), "look.cfg", "model_name = foo\n");
        assert!(!sys.can_load(&lookalike));
    }

    /// An MZ header is not enough on its own: every Windows program has one
    /// too, and DOSBox can run none of them.
    #[test]
    fn tells_a_dos_program_from_a_windows_one() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};

        // A DOS executable: `MZ`, and e_lfanew left as it comes.
        let mut dos = vec![0u8; 0x80];
        dos[..2].copy_from_slice(b"MZ");
        let dos = write_bytes(dir.path(), "demo.exe", &dos);
        assert!(sys.can_load(&dos));
        assert_eq!(core_for(&dos), CORE_NAME_DOSBOX);

        // The same header in front of a PE image is a Windows program: never a
        // DOS one, and never this system's to start - see `super::windows`.
        let mut win = vec![0u8; 0x100];
        win[..2].copy_from_slice(b"MZ");
        win[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        win[0x80..0x84].copy_from_slice(b"PE\0\0");
        let win = write_bytes(dir.path(), "setup32.exe", &win);
        assert_eq!(exe_kind(&win), ExeKind::Windows);
        assert!(!is_dos_program(&win));
        assert!(!sys.can_load(&win));

        // A DOS extender (LE/LX behind the stub) is how the demos were built.
        let mut dos4gw = vec![0u8; 0x100];
        dos4gw[..2].copy_from_slice(b"MZ");
        dos4gw[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        dos4gw[0x80..0x82].copy_from_slice(b"LE");
        let dos4gw = write_bytes(dir.path(), "dos4gw.exe", &dos4gw);
        assert!(sys.can_load(&dos4gw));

        // An offset pointing past the end of the file is a DOS program with a
        // field it never set, not a Windows one whose image we failed to find.
        let mut stub = vec![0u8; 0x80];
        stub[..2].copy_from_slice(b"MZ");
        stub[0x3c..0x40].copy_from_slice(&0x1000u32.to_le_bytes());
        let stub = write_bytes(dir.path(), "stub.exe", &stub);
        assert!(is_dos_program(&stub));

        // A 64K intro packs the two headers into one: `e_lfanew` points at
        // 0x0c, so the PE header's own fields make up the rest of the DOS
        // header. Well inside it, and still a Windows program.
        let mut tiny = vec![0u8; 0x1000];
        tiny[..2].copy_from_slice(b"MZ");
        tiny[0x0c..0x10].copy_from_slice(b"PE\0\0");
        tiny[0x3c..0x40].copy_from_slice(&0x0cu32.to_le_bytes());
        let tiny = write_bytes(dir.path(), "intro64k.exe", &tiny);
        assert_eq!(exe_kind(&tiny), ExeKind::Windows);
        assert!(!is_dos_program(&tiny));

        // Not an executable at all.
        let text = write(dir.path(), "notes.exe", "just a text file\n");
        assert!(!sys.can_load(&text));

        // `.com` has no header, only a size DOS could load into one segment.
        let com = write_bytes(dir.path(), "tiny.com", &[0xcd, 0x20]);
        assert!(sys.can_load(&com));
        let huge = write_bytes(dir.path(), "big.com", &vec![0u8; 0x1_0000]);
        assert!(!sys.can_load(&huge));
        let empty = write_bytes(dir.path(), "nothing.com", &[]);
        assert!(!sys.can_load(&empty));

        // A batch file starts a program, an empty one starts nothing.
        let bat = write(dir.path(), "go.bat", "@echo off\r\ndemo.exe\r\n");
        assert!(sys.can_load(&bat));
        let blank = write(dir.path(), "blank.bat", "");
        assert!(!sys.can_load(&blank));
    }

    /// The two cores split by content, not by system: a machine config drives
    /// PCem, everything else runs on DOSBox's own DOS.
    #[test]
    fn routes_each_kind_of_content_to_its_core() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = write(dir.path(), "486.cfg", "model = ami486\n");
        assert_eq!(core_for(&cfg), CORE_NAME_PCEM);
        assert_eq!(core_for(Path::new("demo.exe")), CORE_NAME_DOSBOX);
        assert_eq!(core_for(Path::new("go.bat")), CORE_NAME_DOSBOX);
        assert_eq!(core_for(Path::new("tiny.com")), CORE_NAME_DOSBOX);
    }

    /// What a release directory holds is rarely one program, and the walk
    /// order says nothing about which of them the release is.
    #[test]
    fn picks_what_the_release_means_to_start() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};

        let release = dir.path().join("crystal");
        fs::create_dir_all(&release).unwrap();
        let mut exe = vec![0u8; 0x80];
        exe[..2].copy_from_slice(b"MZ");
        // Reached first by the walk, and the last thing anyone wants to run.
        write_bytes(&release, "install.exe", &exe);
        write_bytes(&release, "crystal.exe", &exe);
        write_bytes(&release, "zzsetup.exe", &exe);

        let found = sys.get_first_file(&release).unwrap().unwrap();
        assert!(
            found.ends_with("crystal.exe"),
            "picked {found:?} out of the release"
        );

        // A machine config beside the programs describes the whole machine, so
        // it wins - and takes the release to PCem rather than DOSBox.
        write(&release, "crystal.cfg", "model = ami486\n");
        let found = sys.get_first_file(&release).unwrap().unwrap();
        assert!(found.ends_with("crystal.cfg"), "picked {found:?}");
        assert_eq!(core_for(&found), CORE_NAME_PCEM);
    }

    /// A `.bat` beside the program is as often a wrapper printing the .NFO as
    /// it is the way in, and the program starts the same without it.
    #[test]
    fn starts_the_program_rather_than_the_batch_file_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};
        let mut exe = vec![0u8; 0x80];
        exe[..2].copy_from_slice(b"MZ");

        // Named so that neither of them wins on the release name.
        let release = dir.path().join("release");
        fs::create_dir_all(&release).unwrap();
        write(&release, "go.bat", "@echo off\r\ndemo.exe\r\n");
        write_bytes(&release, "trip.exe", &exe);
        let found = sys.get_first_file(&release).unwrap().unwrap();
        assert!(found.ends_with("trip.exe"), "picked {found:?}");

        // With no program to run it is still what starts the release.
        let bare = dir.path().join("bare");
        fs::create_dir_all(&bare).unwrap();
        write(&bare, "go.bat", "@echo off\r\n");
        let found = sys.get_first_file(&bare).unwrap().unwrap();
        assert!(found.ends_with("go.bat"), "picked {found:?}");
    }

    /// A DOS program was linked under a name DOS could type. Anything longer
    /// than 8.3, or with a character DOS never had, was named by whatever
    /// handled the release afterwards.
    #[test]
    fn prefers_a_program_with_a_name_dos_could_have_held() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};
        let mut exe = vec![0u8; 0x80];
        exe[..2].copy_from_slice(b"MZ");

        // Named so that neither wins on the release name, and the long one
        // written first so the walk reaches it before the program.
        let release = dir.path().join("release");
        fs::create_dir_all(&release).unwrap();
        write_bytes(&release, "read me first.exe", &exe);
        write_bytes(&release, "trip.exe", &exe);
        let found = sys.get_first_file(&release).unwrap().unwrap();
        assert!(found.ends_with("trip.exe"), "picked {found:?}");

        // It only breaks a tie: a program named after the release is still
        // what the release is, however that name looks.
        let named = dir.path().join("crystal demo");
        fs::create_dir_all(&named).unwrap();
        write_bytes(&named, "crystal demo.exe", &exe);
        write_bytes(&named, "trip.exe", &exe);
        let found = sys.get_first_file(&named).unwrap().unwrap();
        assert!(found.ends_with("crystal demo.exe"), "picked {found:?}");

        // And an installer stays an installer whatever its name looks like.
        let installer = dir.path().join("installer");
        fs::create_dir_all(&installer).unwrap();
        write_bytes(&installer, "install.exe", &exe);
        write_bytes(&installer, "the whole demo.exe", &exe);
        let found = sys.get_first_file(&installer).unwrap().unwrap();
        assert!(found.ends_with("the whole demo.exe"), "picked {found:?}");
    }

    /// What counts as a name DOS itself could hold.
    #[test]
    fn knows_an_8_3_name_from_a_longer_one() {
        for name in ["demo.exe", "GO.BAT", "a.c", "trip_95.exe", "cw$dpmi.exe"] {
            assert!(is_simple_name(Path::new(name)), "{name} is an 8.3 name");
        }
        for name in [
            // Nine characters in the stem, four in the extension.
            "crystals1.exe",
            "seconddemo.exe",
            "demo.html",
            // Characters DOS never had in a name.
            "read me.exe",
            "démo.exe",
            "demo\u{2b50}.exe",
            // One dot, and on the right side of the name.
            "demo",
            ".exe",
            "readme.txt.exe",
        ] {
            assert!(!is_simple_name(Path::new(name)), "{name} is not 8.3");
        }
        assert!(is_simple_name(Path::new("crystal.exe")));
    }

    /// The extender is shipped beside the program that loads it, so it is the
    /// one `.exe` in a release that never starts anything.
    #[test]
    fn passes_over_an_extender_shipped_beside_the_program() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};
        let mut exe = vec![0u8; 0x80];
        exe[..2].copy_from_slice(b"MZ");

        let release = dir.path().join("release");
        fs::create_dir_all(&release).unwrap();
        // Named so that neither wins on the release name, and written first so
        // that the walk reaches the extender before the demo.
        write_bytes(&release, "DOS4GW.EXE", &exe);
        write_bytes(&release, "trip.exe", &exe);
        let found = sys.get_first_file(&release).unwrap().unwrap();
        assert!(found.ends_with("trip.exe"), "picked {found:?}");

        // On its own it is still all there is to start.
        let bare = dir.path().join("bare");
        fs::create_dir_all(&bare).unwrap();
        write_bytes(&bare, "dos4gw.exe", &exe);
        let found = sys.get_first_file(&bare).unwrap().unwrap();
        assert!(found.ends_with("dos4gw.exe"), "picked {found:?}");
    }

    /// `dos4gw=true` says the release was packed without the extender it was
    /// linked against, which means writing into the release — so it has to be
    /// a copy of one.
    #[test]
    fn puts_the_extender_beside_a_release_asking_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let sys = DosSystem {};
        let mut exe = vec![0u8; 0x80];
        exe[..2].copy_from_slice(b"MZ");

        let release = dir.path().join("crystal");
        fs::create_dir_all(release.join("data")).unwrap();
        write_bytes(&release, "crystal.exe", &exe);
        write(&release.join("data"), "tune.mod", "not really a module");

        // Without the option the release is started where it lies.
        let mut plain = WorkFile::new(release.clone());
        assert!(sys.load(&mut plain).unwrap());
        assert_eq!(plain.path, release.join("crystal.exe"));
        assert!(!plain.is_temporary());

        let meta = HashMap::from([(META_DOS4GW.to_string(), "true".to_string())]);
        let mut wf = WorkFile::new_with_meta(release.clone(), meta);
        assert!(sys.load(&mut wf).unwrap());

        // The whole release came along, data files and all, since that
        // directory is what DOSBox mounts as C:.
        assert!(wf.is_temporary(), "{:?} is not a copy", wf.path);
        assert!(wf.path.ends_with("crystal.exe"), "{:?}", wf.path);
        let copied = wf.path.parent().unwrap();
        assert!(copied.join("data").join("tune.mod").is_file());

        // Whatever happened, it did not happen in the user's own files.
        assert!(!release.join(DOS4GW_EXE).exists());

        // The extender itself is not ours to ship, so only check it landed
        // when there is one in the system dir to land.
        if dos4gw_source().is_file() {
            assert!(
                copied.join(DOS4GW_EXE).is_file(),
                "no extender in {copied:?}"
            );
        }
    }

    /// A release that packed its own extender keeps it: it is the version the
    /// program was built against, and ours would overwrite it.
    #[test]
    fn keeps_an_extender_the_release_brought_itself() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(dir.path(), "dos4gw.exe", "ours");

        let release = dir.path().join("release");
        fs::create_dir_all(&release).unwrap();
        // Lowercase here, uppercase in the copy we would make: the same file
        // to DOS, two files on this filesystem.
        write(&release, "dos4gw.exe", "theirs");
        let program = write(&release, "demo.exe", "MZ");

        place_extender(&WorkFile::new(program), &source).unwrap();
        assert_eq!(
            fs::read_to_string(release.join("dos4gw.exe")).unwrap(),
            "theirs"
        );
        assert_eq!(fs::read_dir(&release).unwrap().count(), 2);

        // With none there, the copy is made under the name DOS uses.
        let empty = dir.path().join("empty");
        fs::create_dir_all(&empty).unwrap();
        let program = write(&empty, "demo.exe", "MZ");
        place_extender(&WorkFile::new(program), &source).unwrap();
        assert_eq!(fs::read_to_string(empty.join(DOS4GW_EXE)).unwrap(), "ours");
    }

    /// How long to give the machine. The XT counts all 640K before it looks
    /// for something to boot, which takes about 35 emulated seconds — it
    /// reaches BASIC around frame 2400.
    const BOOT_FRAME_LIMIT: usize = 4000;

    /// Decode one frame in this many. Reading 2000 character cells is not free
    /// in a debug build, and neither milestone this test looks for is on
    /// screen for less than a second.
    const DECODE_EVERY: usize = 4;

    /// Wall-clock ceiling, so a core that stops producing frames fails the test
    /// instead of hanging it.
    const TIMEOUT: Duration = Duration::from_secs(180);

    /// Boot a real IBM PC/XT, BIOS and all, and read the screen back.
    ///
    /// Ignored because it needs two things this repo does not and cannot ship:
    /// a locally built PCem core (`just pcem-core`), and IBM's copyrighted BIOS
    /// ROMs under `<system dir>/pcem/roms/`. With both in place:
    ///
    ///   DEMARC_CORE_DIR=external/pcem/build-lr/src \
    ///       cargo test boots_an_ibm_xt -- --ignored --nocapture
    ///
    /// With no disks attached the 1981 BIOS falls through to the Cassette
    /// BASIC in ROM, so a successful boot ends on a screen that cannot be
    /// mistaken for anything else.
    #[test]
    #[ignore = "needs a locally built pcem core and IBM BIOS ROMs"]
    fn boots_an_ibm_xt_to_rom_basic() {
        let roms = system_dir().join("pcem").join("roms");
        assert!(
            roms.join("ibmxt").is_dir(),
            "no BIOS ROMs at {} - see testdata/pc/ibmxt.cfg",
            roms.display()
        );
        let font = super::screen::load_font(&roms);

        let cfg = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pc")
            .join("ibmxt.cfg");

        let args = Args::parse_from(["demarc"]);
        let systems = NewSys::new(&args);
        let mut loaded = systems
            .load_file(&cfg, &HashMap::new(), None)
            .expect("failed to load the XT config");
        assert_eq!(loaded.system.name(), "DOS");

        // Both milestones of a real boot: the BIOS sizing memory, then BASIC.
        let mut post_line: Option<String> = None;
        let mut basic_at: Option<usize> = None;
        let mut last = Vec::new();
        let mut frame = 0;
        let deadline = Instant::now() + TIMEOUT;

        while frame < BOOT_FRAME_LIMIT {
            // The core runs on its own thread and `run` only picks up a frame
            // it has already finished, so a `false` means "nothing ready yet",
            // not "no more frames".
            if !loaded.backend.run() {
                assert!(
                    Instant::now() < deadline,
                    "the core stopped producing frames"
                );
                std::thread::yield_now();
                continue;
            }
            frame += 1;
            if frame % DECODE_EVERY != 0 {
                continue;
            }

            let mut text = Vec::new();
            loaded.backend.with_frame(&mut |width, height, pixels| {
                if width >= 640 && height >= 200 {
                    text = super::screen::decode(width, height, pixels, &font);
                }
            });
            if text.is_empty() {
                continue;
            }

            // Keep the newest count rather than the first: the POST writes the
            // running total to the same line as it climbs.
            if let Some(line) = text.iter().find(|l| l.ends_with("KB OK")) {
                post_line = Some(line.clone());
            }
            // The banner alone is not enough: it is printed a character at a
            // time, so sampling during it catches a half-drawn line. "Ok" is
            // BASIC's prompt, and only appears once the interpreter is up and
            // everything above it has been written.
            if text[0].contains("IBM Personal Computer Basic") && text.iter().any(|l| l == "Ok") {
                basic_at = Some(frame);
                last = text;
                break;
            }
            last = text;
        }

        let screen = last.join("\n");
        let frame = basic_at.unwrap_or_else(|| {
            panic!("never reached ROM BASIC in {BOOT_FRAME_LIMIT} frames. Last screen:\n{screen}")
        });
        println!("POST: {}", post_line.as_deref().unwrap_or("(never seen)"));
        println!("booted to ROM BASIC at frame {frame}:\n{screen}");

        // The POST memory count has to agree with `mem_size = 640` in the
        // config — the one number the config and the emulated hardware have to
        // negotiate before the machine will come up at all.
        assert_eq!(
            post_line.as_deref().map(str::trim),
            Some("640 KB OK"),
            "unexpected POST memory count"
        );

        assert!(
            last[1].contains("Copyright IBM Corp 1981"),
            "second line was {:?}",
            last[1]
        );
        assert!(
            last[2].ends_with("Bytes free"),
            "third line was {:?}",
            last[2]
        );
    }

    /// Second Reality's own SETUP screen wants Enter before it starts. Sending
    /// one every second is easier to trust than trying to spot the screen: the
    /// keystrokes before it land at the DOS prompt, where they do nothing.
    const ENTER_EVERY: usize = 60;

    /// Enough for the POST, the FreeDOS boot, JEMMEX, and the demo loading a
    /// megabyte off C: — it reaches the SETUP screen around frame 2000.
    const DEMO_FRAME_LIMIT: usize = 9000;

    /// Wall-clock ceiling for the whole run, so a wedged core fails rather than
    /// hangs. A debug build is a long way off 60 fps here.
    const DEMO_TIMEOUT: Duration = Duration::from_secs(600);

    /// Frames to watch once the demo has switched to its 320x200 mode.
    const ANIMATION_FRAMES: usize = 300;

    /// Boot DOS off a floppy image and run Second Reality from a hard disc.
    ///
    /// Ignored for the same reasons as the XT test — it needs `just pcem-core`
    /// and an AMI 486 BIOS under `<system dir>/pcem/roms/` — plus the ET4000
    /// video BIOS:
    ///
    ///   DEMARC_CORE_DIR=external/pcem/build-lr/src \
    ///       cargo test runs_second_reality -- --ignored --nocapture
    ///
    /// Unlike the XT, there is no text to read at the end: this is a graphics
    /// demo. What it asserts instead is that the machine leaves text mode for
    /// the 320x200 the demo runs in, and that what arrives after that is a
    /// moving picture rather than one frame held still — which cannot happen
    /// without the BIOS, DOS, the hard disc and the video card all working.
    #[test]
    #[ignore = "needs a locally built pcem core and an AMI 486 BIOS"]
    fn runs_second_reality_from_a_dos_hard_disc() {
        let roms = system_dir().join("pcem").join("roms");
        assert!(
            roms.join("ami486").is_dir() && roms.join("et4000.bin").is_file(),
            "no AMI 486 / ET4000 ROMs at {} - see testdata/pc/2ndreality.cfg",
            roms.display()
        );

        let cfg = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pc")
            .join("2ndreality.cfg");

        let args = Args::parse_from(["demarc"]);
        let systems = NewSys::new(&args);
        let mut loaded = systems
            .load_file(&cfg, &HashMap::new(), None)
            .expect("failed to load the Second Reality config");
        assert_eq!(loaded.system.name(), "DOS");

        // Every video mode the machine passes through, in order. The POST and
        // the SETUP screen are 80x25 text; the demo proper is 320x200.
        let mut modes: Vec<(usize, usize, usize)> = Vec::new();
        let mut graphics_at = None;
        let mut seen_text_mode = false;
        let mut frame = 0;
        let deadline = Instant::now() + DEMO_TIMEOUT;

        while frame < DEMO_FRAME_LIMIT {
            if !loaded.backend.run() {
                assert!(
                    Instant::now() < deadline,
                    "the core stopped producing frames"
                );
                std::thread::yield_now();
                continue;
            }
            frame += 1;

            let (width, height) = loaded.backend.get_frame_size();
            if modes.last().map(|&(_, w, h)| (w, h)) != Some((width, height)) {
                modes.push((frame, width, height));
            }
            // PCem starts out at the CGA-ish 656x200 it uses before any card
            // has set a mode, so "not 80x25" is not enough on its own: wait
            // for the ET4000's 720x400 text mode, and only then for the demo
            // to switch away from it.
            if height >= 350 {
                seen_text_mode = true;
            } else if seen_text_mode {
                graphics_at = Some(frame);
                break;
            }
            if frame % ENTER_EVERY == 0 {
                loaded.backend.send_keys(&[(0, RETROK_RETURN)]);
            }
        }

        let modes_seen = modes
            .iter()
            .map(|(at, w, h)| format!("{w}x{h} at {at}"))
            .collect::<Vec<_>>()
            .join(", ");
        let graphics_at = graphics_at.unwrap_or_else(|| {
            panic!("never left text mode in {DEMO_FRAME_LIMIT} frames. Modes: {modes_seen}")
        });
        println!("modes: {modes_seen}");
        println!("in graphics mode at frame {graphics_at}");

        // A demo that has started is a picture that keeps changing. One held
        // frame - a crash back to DOS, or a mode set with nothing behind it -
        // gives a single hash and a near-empty palette.
        let mut hashes = std::collections::HashSet::new();
        let mut colours = std::collections::HashSet::new();
        let mut watched = 0;
        while watched < ANIMATION_FRAMES {
            if !loaded.backend.run() {
                assert!(
                    Instant::now() < deadline,
                    "the core stopped producing frames"
                );
                std::thread::yield_now();
                continue;
            }
            watched += 1;
            hashes.insert(loaded.backend.frame_hash());
            loaded.backend.with_frame(&mut |_, _, pixels| {
                colours.extend(pixels.iter().copied());
            });
        }
        println!(
            "{} distinct frames and {} colours over {ANIMATION_FRAMES} frames",
            hashes.len(),
            colours.len()
        );

        assert!(
            hashes.len() > ANIMATION_FRAMES / 10,
            "only {} distinct frames in {ANIMATION_FRAMES} - the picture is not moving",
            hashes.len()
        );
        assert!(
            colours.len() > 16,
            "only {} distinct colours - the screen is effectively blank",
            colours.len()
        );
    }
}

/// Reading the emulated screen back as text.
///
/// A pixel hash would say the frame changed, not that the machine booted, and
/// it would go stale on any cosmetic change in PCem. Decoding the text instead
/// lets the test assert on what the BIOS actually printed.
#[cfg(test)]
mod screen {
    use std::collections::HashMap;
    use std::path::Path;

    /// An 80x25 text mode is 640x200 pixels, but the card blits its overscan
    /// border too, putting the first character cell 8 pixels in and 4 down.
    const BORDER_X: usize = 8;
    const BORDER_Y: usize = 4;
    pub const COLS: usize = 80;
    pub const ROWS: usize = 25;
    const CELL: usize = 8;

    /// Anything brighter than this in any channel counts as foreground. CGA
    /// text uses a fixed 16-colour palette with nothing near the middle, so
    /// there is no borderline case to get wrong.
    const LIT: u8 = 96;

    /// Maps an 8x8 glyph bitmap back to its character code.
    pub type Font = HashMap<[u8; 8], u8>;

    /// Load the 8x8 CGA font PCem renders text modes with.
    ///
    /// `loadfont(.., FONT_MDA)` in PCem's video.c reads `mda.rom` as four
    /// 2048-byte blocks — the two halves of the 8x14 MDA font, then the thin
    /// and the thick 8x8 CGA fonts. The last block is the one CGA text uses.
    pub fn load_font(roms: &Path) -> Font {
        let rom = std::fs::read(roms.join("mda.rom")).expect("mda.rom (the CGA font) is missing");
        assert!(rom.len() >= 8192, "mda.rom is too short to hold four fonts");

        let mut font = Font::new();
        for ch in 0..=255u8 {
            let off = 6144 + usize::from(ch) * 8;
            let glyph: [u8; 8] = rom[off..off + 8].try_into().unwrap();
            // Codes can share a bitmap — NUL and space are both blank — and
            // first one wins, so a blank cell reads back as NUL, not space.
            // decode() turns both into a space anyway.
            font.entry(glyph).or_insert(ch);
        }
        font
    }

    /// Decode a CGA text-mode frame into its 25 lines of text.
    ///
    /// Cells are matched against the font both as-is and inverted, so the
    /// black-on-white function key bar along the bottom of the BASIC screen
    /// reads like everything else. A cell matching neither becomes `?`.
    pub fn decode(width: usize, height: usize, pixels: &[u32], font: &Font) -> Vec<String> {
        assert!(
            width >= BORDER_X + COLS * CELL && height >= BORDER_Y + ROWS * CELL,
            "frame is {width}x{height}, too small for an 80x25 text mode"
        );

        (0..ROWS)
            .map(|row| {
                let mut line = String::with_capacity(COLS);
                for col in 0..COLS {
                    let mut glyph = [0u8; 8];
                    for (y, bits) in glyph.iter_mut().enumerate() {
                        for x in 0..CELL {
                            let i = (BORDER_Y + row * CELL + y) * width + BORDER_X + col * CELL + x;
                            let [r, g, b, _] = pixels[i].to_ne_bytes();
                            if r > LIT || g > LIT || b > LIT {
                                *bits |= 0x80 >> x;
                            }
                        }
                    }
                    let inverse = glyph.map(|b| !b);
                    match font.get(&glyph).or_else(|| font.get(&inverse)) {
                        Some(&ch) if (0x20..0x7f).contains(&ch) => line.push(ch as char),
                        // Control codes and the line-drawing half of the set:
                        // present, but not what this test reads.
                        Some(_) => line.push(' '),
                        None => line.push('?'),
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }
}
