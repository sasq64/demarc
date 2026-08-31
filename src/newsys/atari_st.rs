use anyhow::Result;
use std::{
    cmp::Reverse,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use crate::utils::{build_m3u, copy_dir_all, find_child, has_extension, sort_disks};

use crate::{newsys::walk_dir, workfile::WorkFile};

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
pub struct AtariStSystem {}

impl AtariStSystem {
    pub fn new() -> Self {
        Self {}
    }
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

impl System for AtariStSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_HATARI
    }

    fn name(&self) -> &'static str {
        "Atari ST"
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("hatari_forcerefresh", "2"),
            ("hatari_start_in_mouse_mode", "false"),
            ("hatari_fastboot", "true"),
            ("hatari_video_crop_overscan", "false"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        let mut exes = vec![];
        for (key, val) in self.default_meta() {
            file.set_meta(key, val);
        }

        // What the release itself asks for, as against what is guessed for it
        // below: a hard drive load replaces a guess, but never a request.
        let machine_given = {
            let machine = file.get_meta_or("hatari_machinetype", "");
            !machine.is_empty() && machine != "date"
        } || file.has_tag("ste");
        let ram_given = file.has_meta("hatari_ramsize")
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
                file.set_meta("hatari_ramsize", "4");
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

        // Hatari sizes its internal "desktop" from the libretro core's
        // retrow/retroh, which only the ST/STE renderer ever updates — the
        // Falcon/TT Videl path never does. Left at the core's low-res default
        // (392x248) every Videl mode is larger than that fake desktop, so
        // hostscreen.c halves it ("too large screen size 640x480 -> divided by
        // 2x2") and draws the shrunken image into the top-left of the frame we
        // are handed. Hires raises it to 832x548, which covers the usual Falcon
        // modes.
        if (machine == "falcon" || machine == "tt") && !file.has_meta("hatari_video_hires") {
            file.set_meta("hatari_video_hires", "true");
        }
        if file.has_tag("requires-4mb") {
            file.set_meta("hatari_ramsize", "4");
        } else if file.has_tag("requires-2mb") {
            file.set_meta("hatari_ramsize", "2");
        } else if file.has_tag("requires-1mb") {
            file.set_meta("hatari_ramsize", "1");
        }

        walk_dir(&file.path.clone(), 4, |path, ext, header| {
            if ["msa", "st"].contains(&ext) {
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
            if !ram_given {
                file.set_meta("hatari_ramsize", "4");
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    /// Pointed at a program on its own, only that program goes on the drive —
    /// whatever else shares the directory with it is none of our business.
    #[test]
    fn bare_program_leaves_its_directory_alone() {
        let dir = tempfile::tempdir().unwrap();
        let prg = dir.path().join("DEMO.EXE");
        write(&prg, &GEMDOS_MAGIC);
        write(&dir.path().join("huge.iso"), b"not ours");

        let wf = build_gemdos_drive(&prg, &prg).unwrap();
        let drive = wf.path.parent().unwrap().join("harddrive");

        assert!(drive.join("AUTO").join(AUTO_PROGRAM).exists());
        assert!(!drive.join("huge.iso").exists());
    }

    /// A directory release brings its files along, and its own `AUTO` folder is
    /// moved aside so ours is the one TOS boots.
    #[test]
    fn directory_release_is_copied_with_its_auto_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("release");
        let prg = base.join("DEMO.EXE");
        write(&prg, &GEMDOS_MAGIC);
        write(&base.join("data").join("music.snd"), b"tune");
        write(&base.join("auto").join("SWAP.PRG"), b"stub");

        let wf = build_gemdos_drive(&prg, &base).unwrap();
        let drive = wf.path.parent().unwrap().join("harddrive");

        assert!(drive.join("data").join("music.snd").exists());
        assert!(drive.join(DISABLED_AUTO).join("SWAP.PRG").exists());
        assert!(drive.join("AUTO").join(AUTO_PROGRAM).exists());
    }

    fn exe(dir: &Path, name: &str, size: usize) -> PathBuf {
        let path = dir.join(name);
        let mut bytes = GEMDOS_MAGIC.to_vec();
        bytes.resize(size, 0);
        write(&path, &bytes);
        path
    }

    /// TalkTalk2: the demo and its readme viewer sit side by side, both named
    /// like programs, and the demo is the bigger one.
    #[test]
    fn biggest_program_of_a_level_wins() {
        let dir = tempfile::tempdir().unwrap();
        let readme = exe(dir.path(), "TLK2READ.PRG", 40_000);
        let demo = exe(dir.path(), "TLKTLK2.PRG", 124_000);

        let exes = [readme, demo.clone()];
        assert_eq!(pick_program(&exes, dir.path(), ""), Some(&demo));
    }

    /// molz: the 651-byte `.tos` loader is what reads the music and hands it to
    /// the 652K part, which is a GEMDOS executable too but named like data.
    #[test]
    fn program_extension_beats_size() {
        let dir = tempfile::tempdir().unwrap();
        let loader = exe(dir.path(), "molz.tos", 651);
        let part = exe(dir.path(), "part1.bin", 652_000);

        let exes = [part, loader.clone()];
        assert_eq!(pick_program(&exes, dir.path(), ""), Some(&loader));
    }

    /// Between programs alike in name, the one nearer the top of the release.
    #[test]
    fn shallowest_program_wins() {
        let dir = tempfile::tempdir().unwrap();
        let top = exe(dir.path(), "DEMO.PRG", 1000);
        let buried = exe(&dir.path().join("PARTS"), "PART1.PRG", 500_000);

        let exes = [buried, top.clone()];
        assert_eq!(pick_program(&exes, dir.path(), ""), Some(&top));
    }

    /// `boot_file` overrides the guess outright, by name or by path, and a
    /// GEMDOS-style path in it still matches.
    #[test]
    fn boot_file_overrides_the_guess() {
        let dir = tempfile::tempdir().unwrap();
        let big = exe(dir.path(), "BIG.PRG", 500_000);
        let wanted = exe(&dir.path().join("DEMO"), "TLKTLK2.PRG", 1000);
        let exes = [big.clone(), wanted.clone()];

        assert_eq!(
            pick_program(&exes, dir.path(), "tlktlk2.prg"),
            Some(&wanted)
        );
        assert_eq!(
            pick_program(&exes, dir.path(), "DEMO/TLKTLK2.PRG"),
            Some(&wanted)
        );
        assert_eq!(
            pick_program(&exes, dir.path(), "DEMO\\TLKTLK2.PRG"),
            Some(&wanted)
        );
        // A name that isn't there falls back to the guess rather than failing.
        assert_eq!(pick_program(&exes, dir.path(), "GONE.PRG"), Some(&big));
    }

    /// A program inside the release's own `AUTO` folder is already started by
    /// it, so the drive is the folder above and nothing is rearranged.
    #[test]
    fn program_in_auto_starts_itself() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("release");
        let prg = base.join("AUTO").join("DEMO.PRG");
        write(&prg, &GEMDOS_MAGIC);

        let wf = build_gemdos_drive(&prg, &base).unwrap();
        let drive = wf.path.parent().unwrap().join("harddrive");

        assert!(drive.join("AUTO").join("DEMO.PRG").exists());
        assert!(!drive.join("AUTO").join(AUTO_PROGRAM).exists());
        assert!(!drive.join(DISABLED_AUTO).exists());
    }
}
