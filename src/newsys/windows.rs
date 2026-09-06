//! Windows releases, run under wine.
//!
//! The whole module is Linux-only — wine and gamescope are — and
//! [`super`] only compiles it there; see the `mod windows` declaration for
//! what a `.exe` becomes on the platforms that don't have it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::dos::{ExeKind, exe_kind};
use super::{System, get_ext, walk_dir};
use crate::backend::Backend;
use crate::libloader;
use crate::retro_emu::RetroCoreThreaded;
use crate::system_dir;
use crate::wine_emu::{
    DEFAULT_DESKTOP, DEFAULT_RES, META_DESKTOP, META_DLL_OVERRIDES, META_RES, PrefixBound, WineEmu,
    dll_overrides, is_yes, open_prefix, wine_command, wine_prefix,
};
use crate::workfile::WorkFile;

/// Meta key: show the demo *inside* demarc rather than on top of it.
///
/// On by default. [`WineEmu`] draws wine straight to the screen, which costs
/// nothing and looks right but leaves the release outside everything demarc
/// does to a picture — no shaders, no grid, no screenshots, no audio. Setting
/// this routes the same wine command through the gamescope libretro core
/// instead, which composites the session headlessly and hands the frames back
/// like any other core. That picture is demarc's to do as it likes with; the
/// price is a readback per frame. See `docs/GAMESCOPE.md`.
pub const META_CAPTURE: &str = "wine_capture";

/// The core that runs the gamescope session. Not on the libretro buildbot, so
/// it only ever resolves through `DEMARC_CORE_DIR` — see [`libloader`].
const CORE_NAME_GAMESCOPE: &str = "gamescope";

/// What holds the words of `gamescope_command` apart.
///
/// A core option is one string, and the command in it is a demo's path with a
/// driver and its arguments around it — full of spaces, brackets and
/// apostrophes, as demo filenames are. Splitting that back into an argv on
/// spaces would break every release with one in its name, so the core splits on
/// this instead when it finds it, and on spaces only when it does not (which is
/// what a `-x gamescope_command=glxgears` typed by hand still wants). ASCII US,
/// the separator that exists for exactly this and cannot appear in a path.
const ARG_SEPARATOR: &str = "\u{1f}";

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

/// DLLs a release ships beside its executable that wine must be told to load
/// instead of its own, as `*`-globs matched against the file name.
///
/// Only d3dx9 for now, and it is the one that keeps coming up. The D3DX helper
/// libraries were never redistributable as part of Windows: a demo that uses
/// one ships that exact build of it, down to the `_37`, and wine's builtin
/// d3dx9 is a reimplementation that is not that build. Left to choose, wine
/// prefers its own and the demo either draws nothing or falls over on a
/// function the real one had.
///
/// The general rule this is a careful slice of — "a DLL a release brought with
/// it is one it meant to use" — is not safe to apply wholesale: a release also
/// ships DLLs wine implements properly and does better with its own of
/// (`d3d9.dll` wrappers, `openal32.dll`, `msvcr*.dll` from a bundled runtime),
/// so the list stays a list.
const NATIVE_DLLS: [&str; 1] = ["d3dx9*.dll"];

/// What wine calls "load the file that is there, not mine": see
/// `WINEDLLOVERRIDES` in wine(1).
const NATIVE: &str = "n";

/// Does `name` match a `*`-glob, ignoring case?
///
/// One `*`, standing for any run of characters including none; anything else in
/// the pattern is a literal. Enough for [`NATIVE_DLLS`] and small enough to read
/// — a pattern with no `*` is a plain comparison.
fn glob_match(name: &str, pattern: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    match pattern.split_once('*') {
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
        None => name == pattern,
    }
}

/// The `WINEDLLOVERRIDES` a release's own files ask for, or nothing if it
/// brought none of the DLLs in [`NATIVE_DLLS`].
///
/// Only the directory the executable is in is looked at — a DLL is loaded from
/// beside the program that wants it, so one buried in `data/` is not one wine is
/// about to pick up anyway.
///
/// The result is wine's own syntax, sorted so the same release always produces
/// the same string: `d3dx9_37,d3dx9_43=n`.
fn native_dll_overrides(dir: &Path) -> Option<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut modules: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            NATIVE_DLLS
                .iter()
                .any(|pattern| glob_match(&name, pattern))
                // wine names the module without its extension, and matches it
                // case-insensitively; lower case is how it is usually written.
                .then(|| {
                    Path::new(&name)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_ascii_lowercase()
                })
        })
        .collect();
    modules.sort();
    modules.dedup();
    (!modules.is_empty()).then(|| format!("{}={NATIVE}", modules.join(",")))
}

/// The smallest and largest either side of a resolution in a file name is
/// allowed to be.
///
/// Two numbers with something between them are not only ever a screen mode:
/// `pack2x2`, a hex `0x1000` and a `demo_2_1` all read the same way to a scan,
/// and none of them is a size to run a demo at. The bounds are what a display
/// could actually be — 320x200 at the bottom, 8K at the top — which throws all
/// three out without needing to understand the rest of the name.
const MIN_SIDE: u32 = 120;
const MAX_SIDE: u32 = 7680;

/// What can sit between the two numbers, most telling first.
///
/// An `x` between two numbers is nearly always a size; an `_` is only a
/// separator and could be holding apart anything, a year and a version
/// included. So a name carrying both — `elevated_1920x1080` — is read by its
/// `x`, and the `_` form is what is left for the names spelled
/// `elevated_1920_1080`.
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
fn res_from_name(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    RES_SEPARATORS
        .iter()
        .find_map(|separators| scan_res(&stem, separators))
}

/// The first `<digits><separator><digits>` in `stem` that could be a screen
/// mode, normalised to `WIDTHxHEIGHT`.
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
        self.handles_ext(path) && is_windows_program(path)
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

        if file.has_tag("512x384") {
            file.set_meta(META_RES, "512x384");
        }

        // A release that names its size in the file name is telling us the one
        // thing that has to be known before it starts - see [`res_from_name`].
        // An entry that sets `wine_res` itself has said it more deliberately,
        // so it wins.
        if !file.has_meta(META_RES)
            && let Some(res) = res_from_name(&target)
        {
            info!("Running {target:?} at {res}, after its name");
            file.set_meta(META_RES, res);
        }

        // A release that carries its own d3dx9 carries it because it needs that
        // build of it - see [`NATIVE_DLLS`]. An entry that has written the
        // overrides out itself has said something more deliberate, so it wins,
        // and adding to it is its author's business: the variable is wine's and
        // is passed through whole.
        if !file.has_meta(META_DLL_OVERRIDES)
            && let Some(dir) = target.parent()
            && let Some(overrides) = native_dll_overrides(dir)
        {
            info!("Running {target:?} with WINEDLLOVERRIDES={overrides}, after its own DLLs");
            file.set_meta(META_DLL_OVERRIDES, overrides);
        }

        // A captured session runs in a wine virtual desktop unless an entry has
        // said otherwise, where [`WineEmu`]'s does not. The difference is that
        // there can be several captured sessions at once — a `--grid` of them —
        // and they share one wine prefix, so they share one wineserver and, on
        // wine's default desktop, one display mode. A demo going fullscreen
        // sets that mode; the next one to try is refused and most of them fall
        // over on the spot. A desktop of its own gives each session a display
        // mode of its own, which is the only thing they were fighting over.
        // See [`crate::wine_emu::desktop_name`].
        //
        // Before `default_meta` fills the key in, so this is the default rather
        // than an override of one, and an entry's own `wine_desktop` still wins.
        if is_yes(&file.get_meta_or(META_CAPTURE, "true")) && !file.has_meta(META_DESKTOP) {
            file.set_meta(META_DESKTOP, "true");
        }

        file.path = target;
        Ok(true)
    }

    /// The size a Windows demo is asked to run at, and the size demarc gives
    /// the gamescope it runs in, plus whether it gets a wine virtual desktop to
    /// run in. Spelled out here rather than left to the backend so they show up
    /// with the rest of an entry's settings.
    fn default_meta(&self) -> HashMap<&str, &str> {
        HashMap::from([
            (META_RES, DEFAULT_RES),
            (META_DESKTOP, if DEFAULT_DESKTOP { "true" } else { "false" }),
        ])
    }

    fn name(&self) -> &'static str {
        "Windows"
    }

    /// Nothing is emulated here either way: the program is run by wine. What
    /// differs is where it lands — on top of demarc through [`WineEmu`], or
    /// inside it through the gamescope core. See [`META_CAPTURE`].
    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        if !is_yes(&path.get_meta_or(META_CAPTURE, "true")) {
            return Ok(Box::new(WineEmu::new(&path.path, path.get_all_meta())?));
        }
        let core = libloader::get_libretro(CORE_NAME_GAMESCOPE)
            .context("Could not load the gamescope core")?;
        // The same claim on the shared prefix [`WineEmu`] takes for a session of
        // its own, and for the same reasons: the first one in clears what a
        // killed demarc left behind, and the last one out closes the prefix.
        // The core is told to keep its hands off it (`gamescope_close_prefix`
        // in [`capture_meta`]) so that `wineserver -k` happens once, here, when
        // nothing is left running in there — otherwise one cell of a grid
        // unloading would end every other cell's wine.
        let prefix = open_prefix()?;
        Ok(Box::new(PrefixBound::new(
            RetroCoreThreaded::new(&core, system_dir(), Some(path), capture_meta(path), false)?,
            prefix,
        )))
    }
}

/// Restate a Windows entry's settings as the gamescope core's options.
///
/// The two name the same things differently: an entry has always said `wine_res`
/// and `wine_desktop`, and the core — which also runs Chrome, and whatever else
/// a session can hold — says `gamescope_resolution` and `gamescope_command`.
/// Translating here keeps the entry vocabulary the one people already write, and
/// keeps `overrides.toml` working unchanged whichever backend runs the release.
///
/// The command is the whole point of doing it here rather than leaving the core
/// to work it out from the file name. Left alone the core runs `wine <exe>`,
/// which is a demo sitting on its setup dialog with nobody to answer it; what it
/// is given instead is exactly the command [`WineEmu`] would have run — the
/// dialog driver, the resolution to pick, the virtual desktop if one was asked
/// for — built in one place by [`crate::wine_emu::wine_command`] so the two
/// backends cannot drift apart. See `docs/GAMESCOPE.md`.
///
/// Anything already set explicitly wins, so `-x gamescope_command=...` still
/// overrides the whole thing, which is how the core gets tested against a client
/// that is not wine at all.
fn capture_meta(path: &WorkFile) -> HashMap<String, String> {
    let mut meta = path.get_all_meta();

    match wine_command(&path.path, &meta) {
        Ok(cmd) => {
            // The size the driver is about to ask the dialog for, which is the
            // size the session has to be. Not read from `wine_res` directly:
            // `pick` is not a size, and the one it stands for is the backend's
            // to decide.
            meta.entry("gamescope_resolution".into())
                .or_insert_with(|| format!("{}x{}", cmd.width, cmd.height));
            meta.entry("gamescope_command".into())
                .or_insert_with(|| cmd.argv.join(ARG_SEPARATOR));
        }
        Err(err) => {
            // Only a release that has gone missing between being unpacked and
            // being started gets here. The core can still make a command out of
            // the path it is handed, so let it: a demo with an unanswered dialog
            // is better than no demo at all.
            warn!(
                "Could not work out the wine command for {:?}: {err}",
                path.path
            );
            meta.entry("gamescope_command".into())
                .or_insert_with(|| "wine".into());
        }
    }

    // Whatever DLLs the release brought with it, or an entry asked for by hand.
    // The core exports it for the same reason [`WineEmu`] does — it is wine
    // inside there either way — and it is spelled out here so both backends read
    // it off the one key.
    if let Some(overrides) = dll_overrides(&meta) {
        meta.entry("gamescope_wine_dll_overrides".into())
            .or_insert(overrides);
    }

    // The same prefix [`WineEmu`] uses, so a release prepared under one backend is
    // still prepared under the other and neither goes near the user's own `~/.wine`.
    if !meta.contains_key("gamescope_wineprefix")
        && let Ok(prefix) = wine_prefix()
    {
        meta.insert(
            "gamescope_wineprefix".into(),
            prefix.to_string_lossy().into_owned(),
        );
    }

    // Whose job it is to end wine in that prefix. Not the core's: `wineserver -k`
    // ends every wine process in a prefix at once, and each core instance can
    // only ever know about its own session, so a grid of them would close the
    // prefix out from under each other. demarc counts the sessions and closes it
    // when the last one goes — see [`crate::wine_emu::PrefixGuard`].
    meta.entry("gamescope_close_prefix".into())
        .or_insert_with(|| "false".into());

    meta
}

#[cfg(test)]
#[path = "tests/windows_tests.rs"]
mod tests;
