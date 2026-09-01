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
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use std::fs;

    #[cfg(target_os = "linux")]
    fn write_bytes(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    /// A Windows program is an MZ like any other; what makes it one is the PE
    /// image the stub points at.
    #[test]
    fn tells_a_windows_program_from_a_dos_one() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, body: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, body).unwrap();
            path
        };
        let sys = WindowsSystem {};

        let mut win = vec![0u8; 0x100];
        win[..2].copy_from_slice(b"MZ");
        win[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        win[0x80..0x84].copy_from_slice(b"PE\0\0");
        let win = write("setup32.exe", &win);
        assert!(is_windows_program(&win));
        // Only where there is a wine to run it.
        assert_eq!(sys.can_load(&win), CAN_RUN_WINDOWS);

        // A 64K intro packs the two headers into one: `e_lfanew` points at
        // 0x0c, so the PE header's own fields make up the rest of the DOS
        // header. Well inside it, and still a Windows program.
        let mut tiny = vec![0u8; 0x1000];
        tiny[..2].copy_from_slice(b"MZ");
        tiny[0x0c..0x10].copy_from_slice(b"PE\0\0");
        tiny[0x3c..0x40].copy_from_slice(&0x0cu32.to_le_bytes());
        let tiny = write("intro64k.exe", &tiny);
        assert!(is_windows_program(&tiny));

        // A plain DOS executable, and a DOS extender (LE/LX behind the stub):
        // neither is ours.
        let mut dos = vec![0u8; 0x80];
        dos[..2].copy_from_slice(b"MZ");
        let dos = write("demo.exe", &dos);
        assert!(!is_windows_program(&dos));
        assert!(!sys.can_load(&dos));

        let mut dos4gw = vec![0u8; 0x100];
        dos4gw[..2].copy_from_slice(b"MZ");
        dos4gw[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        dos4gw[0x80..0x82].copy_from_slice(b"LE");
        let dos4gw = write("dos4gw.exe", &dos4gw);
        assert!(!is_windows_program(&dos4gw));

        // An offset pointing past the end of the file is a DOS program with a
        // field it never set, not a Windows one whose image we failed to find.
        let mut stub = vec![0u8; 0x80];
        stub[..2].copy_from_slice(b"MZ");
        stub[0x3c..0x40].copy_from_slice(&0x1000u32.to_le_bytes());
        let stub = write("stub.exe", &stub);
        assert!(!is_windows_program(&stub));

        // Not an executable at all.
        let text = write("notes.exe", b"just a text file\n");
        assert!(!is_windows_program(&text));
    }

    /// A release directory holding a Windows program is the release, and on
    /// Linux it is ours to start.
    #[test]
    #[cfg(target_os = "linux")]
    fn claims_a_windows_release_for_wine() {
        let dir = tempfile::tempdir().unwrap();
        let sys = WindowsSystem {};

        let release = dir.path().join("kotpg");
        fs::create_dir_all(&release).unwrap();
        let mut pe = vec![0u8; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");
        // The one thing in the release nobody wants to start, reached first.
        write_bytes(&release, "install.exe", &pe);
        write_bytes(&release, "kotpg.exe", &pe);

        let mut wf = WorkFile::new(release.clone());
        assert!(sys.load(&mut wf).unwrap());
        assert!(wf.path.ends_with("kotpg.exe"), "picked {:?}", wf.path);

        // The size the dialog driver is told to pick, unless an entry says
        // otherwise - see `crate::wine_emu`.
        assert_eq!(
            sys.default_meta().get(crate::wine_emu::META_RES),
            Some(&"800x600")
        );
    }

    /// A Windows release often names the size it was built for, and that name
    /// is the only place the size is written down.
    #[test]
    #[cfg(target_os = "linux")]
    fn takes_the_resolution_out_of_a_windows_program_name() {
        let dir = tempfile::tempdir().unwrap();
        let sys = WindowsSystem {};
        let mut pe = vec![0u8; 0x100];
        pe[..2].copy_from_slice(b"MZ");
        pe[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        pe[0x80..0x84].copy_from_slice(b"PE\0\0");

        let release = dir.path().join("fr08");
        fs::create_dir_all(&release).unwrap();
        write_bytes(&release, "fr08_1920x1080.exe", &pe);

        let mut wf = WorkFile::new(release.clone());
        assert!(sys.load(&mut wf).unwrap());
        assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "1920x1080");

        // What the entry says was decided by a person, and beats a file name.
        let meta = HashMap::from([(crate::wine_emu::META_RES.to_string(), "800x600".to_string())]);
        let mut wf = WorkFile::new_with_meta(release, meta);
        assert!(sys.load(&mut wf).unwrap());
        assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "800x600");

        // The same release, spelled the other way.
        let elevated = dir.path().join("elevated");
        fs::create_dir_all(&elevated).unwrap();
        write_bytes(&elevated, "elevated_1440_900.exe", &pe);
        let mut wf = WorkFile::new(elevated);
        assert!(sys.load(&mut wf).unwrap());
        assert_eq!(wf.get_meta_or(crate::wine_emu::META_RES, ""), "1440x900");

        // A DOS program is not this system's, so nothing here fills anything
        // in for it - it runs under DOSBox, which has no such setting.
        let dos = dir.path().join("dos");
        fs::create_dir_all(&dos).unwrap();
        let mut mz = vec![0u8; 0x80];
        mz[..2].copy_from_slice(b"MZ");
        write_bytes(&dos, "demo_640x480.exe", &mz);
        let mut wf = WorkFile::new(dos);
        assert!(!sys.load(&mut wf).unwrap());
        assert!(!wf.has_meta(crate::wine_emu::META_RES));
    }

    /// The scan has to tell a screen mode from every other reason two numbers
    /// end up next to each other in a name.
    #[test]
    fn reads_a_resolution_only_where_a_name_holds_one() {
        let res = |name: &str| res_from_name(Path::new(name));

        assert_eq!(res("bla_1920x1080.exe").as_deref(), Some("1920x1080"));
        assert_eq!(res("demo-640X480.exe").as_deref(), Some("640x480"));
        // Digits running straight into the rest of the name are still digits.
        assert_eq!(res("vga320x200.exe").as_deref(), Some("320x200"));
        assert_eq!(res("intro_512x384_final.exe").as_deref(), Some("512x384"));

        // The same sizes spelled with an underscore between them.
        assert_eq!(res("elevated_1920_1080.exe").as_deref(), Some("1920x1080"));
        assert_eq!(res("elevated_1280_720.exe").as_deref(), Some("1280x720"));
        assert_eq!(res("demo_800_600_final.exe").as_deref(), Some("800x600"));
        // With both to go on, the `x` is the one that means a size.
        assert_eq!(res("party_2009_640x480.exe").as_deref(), Some("640x480"));

        // Not sizes: a pack count, a version, a hex address, a texture.
        assert_eq!(res("pack2x2.exe"), None);
        assert_eq!(res("demo_2_1.exe"), None);
        assert_eq!(res("loader_0x1000.exe"), None);
        assert_eq!(res("atlas_16384x16384.exe"), None);
        // Digits on one side of the separator only.
        assert_eq!(res("directx9.exe"), None);
        assert_eq!(res("64x.exe"), None);
        assert_eq!(res("demo_1024_final.exe"), None);
        assert_eq!(res("demo.exe"), None);
    }
}
