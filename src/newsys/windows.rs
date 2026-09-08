//! Windows releases, run under wine.

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
    DEFAULT_DESKTOP, DEFAULT_GL_COMPAT, DEFAULT_RES, GL_COMPAT_OVERRIDE, META_DESKTOP,
    META_GL_COMPAT, META_RES, WineEmu, close_prefix, dll_overrides, gl_compat, wine_command,
    wine_prefix,
};
use crate::workfile::WorkFile;

const CORE_NAME_GAMESCOPE: &str = "gamescope";

/// What holds the words of `gamescope_command` apart.
const ARG_SEPARATOR: &str = "\u{1f}";

pub struct WindowsSystem {}

/// Does this look like a Windows program?
///
/// The exact complement of the `.exe` half of the DOS system's own check, read
/// from the same header — see [`exe_kind`].
fn is_windows_program(path: &Path) -> bool {
    get_ext(path) == "exe" && exe_kind(path) == ExeKind::Windows
}

/// How much we want to start a given program, biggest first.
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

    fn get_first_file(&self, dir: &Path) -> Result<Option<PathBuf>> {
        self.pick_target(dir)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let Some(target) = self.pick_target(file)? else {
            return Ok(false);
        };

        for tag in ["512x384", "320x200", "640x480", "1024x768", "1280x720"] {
            if file.has_tag(tag) {
                file.set_meta(META_RES, tag);
                break;
            }
        }

        // Native for all D3D seems to work
        file.set_meta("gamescope_dll_overrides", "d3d*=n,b");
        file.set_meta("gamescope_wine_dll_overrides", "d3d*=n,b");

        if !file.has_meta(META_RES)
            && let Some(res) = res_from_name(&target)
        {
            info!("Running {target:?} at {res}, after its name");
            file.set_meta(META_RES, res);
        }

        file.path = target;
        Ok(true)
    }

    /// The size a Windows demo is asked to run at, and the size demarc gives
    /// the gamescope it runs in, plus whether it gets a wine virtual desktop to
    /// run in and whether Mesa is asked for a compatibility profile. Spelled out
    /// here rather than left to the backend so they show up with the rest of an
    /// entry's settings.
    fn default_meta(&self) -> HashMap<&str, &str> {
        HashMap::from([
            (META_RES, DEFAULT_RES),
            (META_DESKTOP, if DEFAULT_DESKTOP { "true" } else { "false" }),
            (
                META_GL_COMPAT,
                if DEFAULT_GL_COMPAT { "true" } else { "false" },
            ),
        ])
    }

    fn name(&self) -> &'static str {
        "Windows"
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        if path.is_disabled("wine_capture") {
            // Use old WineEmu that runs outside of demarc
            return Ok(Box::new(WineEmu::new(&path.path, path.get_all_meta())?));
        }
        let core = libloader::get_libretro(CORE_NAME_GAMESCOPE)
            .context("Could not load the gamescope core")?;

        if let Ok(prefix) = wine_prefix() {
            close_prefix(&prefix);
        }
        Ok(Box::new(RetroCoreThreaded::new(
            &core,
            system_dir(),
            Some(path),
            capture_meta(path),
            false,
        )?))
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

    // `wine_gl_compat` is a yes/no an entry's author can answer; what the core
    // exports is the Mesa variable itself, so the two are not the same key and
    // the translation happens here with the rest of them. Only set when the
    // answer is yes: unset is what leaves the demo's own profile request alone.
    if gl_compat(&meta) {
        meta.entry("gamescope_mesa_gl_version_override".into())
            .or_insert_with(|| GL_COMPAT_OVERRIDE.to_string());
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

    meta
}

#[cfg(test)]
#[path = "tests/windows_tests.rs"]
mod tests;
