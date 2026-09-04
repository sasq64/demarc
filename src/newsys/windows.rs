use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
#[cfg(target_os = "linux")]
use tracing::info;

use super::dos::{ExeKind, exe_kind};
use super::{System, get_ext, walk_dir};
use crate::backend::Backend;
#[cfg(target_os = "linux")]
use crate::wine_emu::WineEmu;
use crate::workfile::WorkFile;

/// Win32 programs, run rather than emulated.
///
/// A Windows release is the same `.exe` a DOS one is, with a `PE` image behind
/// the DOS stub — see [`exe_kind`], which reads the header for both sides. What
/// happens to it afterwards has nothing in common with the DOS half: there is
/// no core and no emulated machine, only wine running the program on top of
/// demarc. See [`crate::wine_emu`].
///
/// `wine_res` sets the size it runs at, and a release that names its own size —
/// `demo_1920x1080.exe` — fills that in by itself, see [`res_from_name`].
pub struct WindowsSystem {}

/// Whether a Windows program can be started at all here.
///
/// wine and gamescope are Linux-only, so everywhere else a `.exe` with a `PE`
/// image in it is something nothing can run — and claiming it would take the
/// release away from the picture and music systems that can at least show what
/// it shipped beside the program.
const CAN_RUN_WINDOWS: bool = cfg!(target_os = "linux");

/// Does this look like a Windows program?
///
/// The exact complement of the `.exe` half of the DOS system's own check, read
/// from the same header — see [`exe_kind`].
fn is_windows_program(path: &Path) -> bool {
    get_ext(path) == "exe" && exe_kind(path) == ExeKind::Windows
}

/// How much we want to start a given program, biggest first.
///
/// A release is usually a directory holding one program worth running and
/// several that aren't — an installer, a setup tool, a viewer for the .NFO —
/// and the walk reaches them in whatever order the filesystem gives. So rank
/// them: the file named after the release is what the release is, and anything
/// called INSTALL or SETUP is the one thing we know we don't want.
fn launch_rank(path: &Path, release: &str) -> i32 {
    let stem = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();

    let mut rank = 0;
    if !release.is_empty() && stem == release {
        rank += 10;
    }
    if ["install", "setup", "config", "uninstal", "readme"].contains(&stem.as_str()) {
        rank -= 20;
    }
    rank
}

/// The smallest and largest either side of a resolution in a file name is
/// allowed to be.
///
/// Two numbers with something between them are not only ever a screen mode:
/// `pack2x2`, a hex `0x1000` and a `demo_2_1` all read the same way to a scan,
/// and none of them is a size to run a demo at. The bounds are what a display
/// could actually be — 320x200 at the bottom, 8K at the top — which throws all
/// three out without needing to understand the rest of the name.
// Only consumed from the `wine_res` handling below, which is Linux-only; kept
// available everywhere so `reads_a_resolution_only_where_a_name_holds_one`
// exercises the same parsing on every platform.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const MIN_SIDE: u32 = 120;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const MAX_SIDE: u32 = 7680;

/// What can sit between the two numbers, most telling first.
///
/// An `x` between two numbers is nearly always a size; an `_` is only a
/// separator and could be holding apart anything, a year and a version
/// included. So a name carrying both — `elevated_1920x1080` — is read by its
/// `x`, and the `_` form is what is left for the names spelled
/// `elevated_1920_1080`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const RES_SEPARATORS: [&[char]; 2] = [&['x', 'X'], &['_']];

/// Read the resolution a Windows release named itself after.
///
/// A demo built for one size often says so in the file name —
/// `demo_1920x1080.exe`, `elevated_1440_900.exe` — and that is the only place
/// it says it. It matters because the size has to be settled before the demo
/// starts: the dialog driver picks the mode by matching what demarc asked for
/// against the labels in the setup dialog, and gamescope is given a session
/// that size (see [`crate::wine_emu`]).
///
/// The digits are taken as they lie, so `vga640x480` reads as well as
/// `demo_640x480` does; only the numbers have to make sense, per [`MIN_SIDE`].
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn res_from_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    RES_SEPARATORS
        .iter()
        .find_map(|separators| scan_res(&stem, separators))
}

/// The first `<digits><separator><digits>` in `stem` that could be a screen
/// mode, normalised to `WIDTHxHEIGHT`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn scan_res(stem: &str, separators: &[char]) -> Option<String> {
    let bytes = stem.as_bytes();
    for (i, sep) in stem.match_indices(separators) {
        // Both runs stop at the first byte that isn't a digit, so the number
        // is whatever lies against the separator: `vga640x480` reads as
        // 640x480, and the name in front of it is no business of ours.
        let start = bytes[..i]
            .iter()
            .rposition(|c| !c.is_ascii_digit())
            .map_or(0, |p| p + 1);
        let rest = i + sep.len();
        let end = rest
            + bytes[rest..]
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(bytes.len() - rest);
        // Digits on both sides, or the separator is part of a word rather than
        // between two numbers.
        let (Ok(width), Ok(height)) = (
            stem[start..i].parse::<u32>(),
            stem[rest..end].parse::<u32>(),
        ) else {
            continue;
        };
        if (MIN_SIDE..=MAX_SIDE).contains(&width) && (MIN_SIDE..=MAX_SIDE).contains(&height) {
            return Some(format!("{width}x{height}"));
        }
    }
    None
}

impl WindowsSystem {
    /// Which of the files in a release is the one to start.
    ///
    /// The programs are ranked against each other and the best one taken — see
    /// [`launch_rank`]. `dir` names the release, which is how a program named
    /// after it is recognised, and may equally be a single file.
    fn pick_target(&self, dir: &Path) -> Result<Option<PathBuf>> {
        let release = dir
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        let mut best: Option<(i32, PathBuf)> = None;
        walk_dir(dir, 0, |path, _ext, _| {
            if !self.can_load(path) {
                return Ok(());
            }
            let rank = launch_rank(path, &release);
            if best.as_ref().is_none_or(|(top, _)| rank > *top) {
                best = Some((rank, path.to_owned()));
            }
            Ok(())
        })?;
        Ok(best.map(|b| b.1))
    }
}

impl System for WindowsSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["exe"]
    }

    fn can_load(&self, path: &Path) -> bool {
        CAN_RUN_WINDOWS && self.handles_ext(path) && is_windows_program(path)
    }

    /// The default walks for the first file it can load, in whatever order the
    /// filesystem hands them over — which for a release directory holding
    /// several programs is not a choice at all. See
    /// [`WindowsSystem::pick_target`].
    fn get_first_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        self.pick_target(dir)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let Some(target) = self.pick_target(file)? else {
            return Ok(false);
        };

        #[cfg(target_os = "linux")]
        if file.has_tag("512x384") {
            file.set_meta(crate::wine_emu::META_RES, "512x384");
        }

        // A release that names its size in the file name is telling us the one
        // thing that has to be known before it starts - see [`res_from_name`].
        // An entry that sets `wine_res` itself has said it more deliberately,
        // so it wins.
        #[cfg(target_os = "linux")]
        if !file.has_meta(crate::wine_emu::META_RES)
            && let Some(res) = res_from_name(&target)
        {
            info!("Running {target:?} at {res}, after its name");
            file.set_meta(crate::wine_emu::META_RES, res);
        }

        file.path = target;
        Ok(true)
    }

    /// The size a Windows demo is asked to run at, and the size demarc gives
    /// the gamescope it runs in, plus whether it gets a wine virtual desktop to
    /// run in. Spelled out here rather than left to the backend so they show up
    /// with the rest of an entry's settings.
    fn default_meta(&self) -> HashMap<&str, &str> {
        #[allow(unused_mut)]
        let mut meta: HashMap<&str, &str> = HashMap::new();
        #[cfg(target_os = "linux")]
        {
            meta.insert(crate::wine_emu::META_RES, crate::wine_emu::DEFAULT_RES);
            meta.insert(
                crate::wine_emu::META_DESKTOP,
                if crate::wine_emu::DEFAULT_DESKTOP {
                    "true"
                } else {
                    "false"
                },
            );
        }
        meta
    }

    fn name(&self) -> &'static str {
        "Windows"
    }

    /// Nothing is emulated here: the program is run, on top of demarc, by
    /// [`WineEmu`].
    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        #[cfg(target_os = "linux")]
        return Ok(Box::new(WineEmu::new(&path.path, path.get_all_meta())?));
        // `can_load` said no everywhere else, so this is only reachable by
        // asking for a Windows program by hand.
        #[cfg(not(target_os = "linux"))]
        anyhow::bail!("{:?} needs wine, which demarc only has on Linux", path.path);
    }
}

#[cfg(test)]
#[path = "tests/windows_tests.rs"]
mod tests;
