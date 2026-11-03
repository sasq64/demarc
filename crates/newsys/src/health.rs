//! `demarc --check`: what demarc relies on outside its own binary, and what to
//! do about whatever is wrong with it. Only looks: nothing here changes a thing.

use std::fmt;

use crate::{fetch, ffmpeg_emu, newsys};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    /// Something to know, not something to judge.
    Info,
    Ok,
    /// demarc runs, but with less than it could.
    Warn,
    Missing,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Info => "info",
            Status::Ok => "ok",
            Status::Warn => "WARN",
            Status::Missing => "MISSING",
        }
    }
}

pub struct Line {
    pub status: Status,
    pub name: &'static str,
    pub detail: String,
    pub fix: Option<String>,
}

impl Line {
    fn new(status: Status, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status,
            name,
            detail: detail.into(),
            fix: None,
        }
    }

    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

pub struct Report {
    pub sections: Vec<(String, Vec<Line>)>,
}

impl Report {
    /// Nothing missing. Warnings don't count: `--check` is usable from a script.
    pub fn ok(&self) -> bool {
        self.lines().all(|line| line.status != Status::Missing)
    }

    fn lines(&self) -> impl Iterator<Item = &Line> {
        self.sections.iter().flat_map(|(_, lines)| lines)
    }

    /// Each thing to do once, however many lines it would repair.
    fn fixes(&self) -> Vec<&str> {
        let mut fixes = vec![];
        for fix in self.lines().filter_map(|line| line.fix.as_deref()) {
            if !fixes.contains(&fix) {
                fixes.push(fix);
            }
        }
        fixes
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (title, lines) in &self.sections {
            writeln!(f, "{title}")?;
            for line in lines {
                let (status, name) = (line.status.label(), line.name);
                writeln!(f, "  {status:<8}{name:<21}{}", line.detail)?;
            }
            writeln!(f)?;
        }
        let fixes = self.fixes();
        if fixes.is_empty() {
            return write!(f, "Everything is in order.");
        }
        write!(f, "To fix:")?;
        for fix in fixes {
            write!(f, "\n  - {fix}")?;
        }
        Ok(())
    }
}

fn human_size(size: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{size} bytes"),
        _ => format!("{value:.1} {}", UNITS[unit]),
    }
}

fn cache_lines() -> Vec<Line> {
    let temp = newsys::temp_cache_sizes();
    let parts: Vec<_> = temp
        .iter()
        .map(|(name, size)| format!("{name} {}", human_size(*size)))
        .collect();
    let total = temp.iter().map(|(_, size)| size).sum();
    vec![
        Line::new(
            Status::Info,
            "Download cache",
            human_size(fetch::cache_size()),
        ),
        Line::new(
            Status::Info,
            "Temp cache",
            format!("{} ({})", human_size(total), parts.join(", ")),
        ),
    ]
}

fn ffmpeg_line() -> Line {
    if ffmpeg_emu::is_available() {
        Line::new(Status::Ok, "ffmpeg", "ffmpeg and ffprobe run")
    } else {
        Line::new(Status::Warn, "ffmpeg", "not on PATH: video files cannot play")
            .fix("Install ffmpeg")
    }
}

fn rife_line() -> Line {
    match rife::lib_candidates().into_iter().find(|path| path.is_file()) {
        Some(path) => Line::new(Status::Ok, "RIFE", path.display().to_string()),
        None => Line::new(
            Status::Info,
            "RIFE",
            format!("no {}: --frame-convert=rife is unavailable", rife::LIB_NAME),
        ),
    }
}

/// Run every check. Takes a moment: it starts a test sandbox and a few tools.
pub fn check() -> Report {
    let mut sections = vec![];
    #[cfg(target_os = "linux")]
    sections.extend(linux::sections());
    sections.push(("Video".to_string(), vec![ffmpeg_line(), rife_line()]));
    let cache = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    sections.push((
        format!("Caches ({})", cache.join("demarc").display()),
        cache_lines(),
    ));
    Report { sections }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    use super::{Line, Status};
    use crate::wine::{self, find_tool, wine_prefix};
    use crate::{libloader, system_dir, utils, wine_sandbox};

    const SETUP: &str = "Run `demarc --setup-wine`";

    /// Verbs, and the files in `drive_c` that show one really ran. The log
    /// winetricks keeps says less: setup deletes it, and it can lag the prefix.
    pub(super) const FONTS: &[(&str, &str)] = &[
        ("andale", "andalemo.ttf"),
        ("arial", "arial.ttf"),
        ("comicsans", "comic.ttf"),
        ("courier", "cour.ttf"),
        ("georgia", "georgia.ttf"),
        ("impact", "impact.ttf"),
        ("times", "times.ttf"),
        ("trebuchet", "trebuc.ttf"),
        ("verdana", "verdana.ttf"),
        ("webdings", "webdings.ttf"),
        ("Selawik", "selawk.ttf"),
    ];

    pub(super) const NATIVE_DLLS: &[(&str, &[&str])] = &[
        (
            "dxvk",
            &[
                "windows/system32/d3d11.dll",
                "windows/system32/dxgi.dll",
                "windows/syswow64/d3d9.dll",
                "windows/syswow64/d3d11.dll",
                "windows/syswow64/dxgi.dll",
            ],
        ),
        ("d3dx9", &["windows/syswow64/d3dx9_43.dll"]),
        ("d3dx10", &["windows/syswow64/d3dx10_43.dll"]),
        ("vkd3d", &["windows/syswow64/d3d12.dll"]),
        ("d3dx11_42", &["windows/syswow64/d3dx11_42.dll"]),
        ("d3dx11_43", &["windows/syswow64/d3dx11_43.dll"]),
        ("d3dcompiler_42", &["windows/syswow64/d3dcompiler_42.dll"]),
        ("d3dcompiler_43", &["windows/syswow64/d3dcompiler_43.dll"]),
        ("d3dcompiler_46", &["windows/syswow64/d3dcompiler_46.dll"]),
        (
            "d3dcompiler_47",
            &[
                "windows/system32/d3dcompiler_47.dll",
                "windows/syswow64/d3dcompiler_47_native.dll",
            ],
        ),
        (
            "d3dcsx_43",
            &[
                "windows/system32/d3dcsx_43.dll",
                "windows/syswow64/d3dcsx_43.dll",
            ],
        ),
        ("mfc42", &["windows/syswow64/mfc42.dll"]),
        ("msvcp60", &["windows/syswow64/msvcp60.dll"]),
        ("ffdshow", &["windows/syswow64/ff_vfw.dll"]),
        (
            "speechsdk",
            &["Program Files (x86)/Microsoft Speech SDK 5.1/Bin/TTSEng.dll"],
        ),
    ];

    /// Files `setup-wine.sh` copies from `prefix/`, and where they go.
    pub(super) const PROXIES: &[(&str, &str)] = &[
        ("dcomp.dll", "system32"),
        ("winmm.dll", "syswow64"),
        ("kernelbase.dll", "syswow64"),
        ("dx8vb.dll", "syswow64"),
        ("opengl32.dll", "syswow64"),
        ("d3d10_1.dll", "syswow64"),
        ("d3dcompiler_47.dll", "syswow64"),
    ];

    pub(super) const EXTRAS: &[(&str, &str)] = &[
        ("tssoft32.acm", "syswow64"),
        ("tsd32.dll", "syswow64"),
        ("ir32_32.dll", "syswow64"),
        ("gm.dls", "syswow64/drivers"),
        ("gm.dls", "system32/drivers"),
    ];

    /// `(what, hive, key, value)`, spelled the way wine saves its hives.
    const REGISTRY: &[(&str, &str, &str, &str)] = &[
        (
            "Segoe UI replacement",
            "user.reg",
            r"Software\\Wine\\Fonts\\Replacements",
            r#""Segoe UI"="Selawik""#,
        ),
        (
            "UseEGL",
            "user.reg",
            r"Software\\Wine\\X11 Driver",
            r#""UseEGL"="N""#,
        ),
        (
            "LogPixels",
            "user.reg",
            r"Control Panel\\Desktop",
            r#""LogPixels"=dword:00000060"#,
        ),
        (
            "wineusb.sys override",
            "user.reg",
            r"Software\\Wine\\DllOverrides",
            r#""wineusb.sys"="""#,
        ),
        (
            "UE4 win10 override",
            "user.reg",
            r"Software\\Wine\\AppDefaults\\ED3-Win64-Shipping.exe",
            r#""Version"="win10""#,
        ),
        (
            "winebus DisableHidraw",
            "system.reg",
            r"System\\ControlSet001\\Services\\winebus",
            r#""DisableHidraw"=dword:00000001"#,
        ),
        (
            "winebus Enable SDL",
            "system.reg",
            r"System\\ControlSet001\\Services\\winebus",
            r#""Enable SDL"=dword:00000000"#,
        ),
        (
            "winebus DisableInput",
            "system.reg",
            r"System\\ControlSet001\\Services\\winebus",
            r#""DisableInput"=dword:00000001"#,
        ),
        (
            "gm.dls path",
            "system.reg",
            r"Software\\Microsoft\\DirectMusic",
            r#""GMFilePath"="C:\\windows\\system32\\drivers\\gm.dls""#,
        ),
        (
            "tssoft32 codec",
            "system.reg",
            r"Software\\Wow6432Node\\Microsoft\\Windows NT\\CurrentVersion\\Drivers32",
            r#""msacm.tssoft32"="tssoft32.acm""#,
        ),
        (
            "tssoft32 codec (64-bit)",
            "system.reg",
            r"Software\\Microsoft\\Windows NT\\CurrentVersion\\Drivers32",
            r#""msacm.tssoft32"="tssoft32.acm""#,
        ),
        (
            "Indeo iv31 codec",
            "system.reg",
            r"Software\\Wow6432Node\\Microsoft\\Windows NT\\CurrentVersion\\Drivers32",
            r#""vidc.iv31"="ir32_32.dll""#,
        ),
        (
            "Indeo iv32 codec",
            "system.reg",
            r"Software\\Wow6432Node\\Microsoft\\Windows NT\\CurrentVersion\\Drivers32",
            r#""vidc.iv32"="ir32_32.dll""#,
        ),
        (
            "MP42 codec",
            "system.reg",
            r"Software\\Wow6432Node\\Microsoft\\Windows NT\\CurrentVersion\\Drivers32",
            r#""vidc.mp42"="ff_vfw.dll""#,
        ),
    ];

    /// Where the core looks for Chrome (`ChromeBinary` in `core.cpp`).
    const CHROME_PATHS: [&str; 4] = [
        "/usr/bin/chromium",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/google-chrome",
        "/usr/bin/chrome",
    ];

    pub(super) fn sections() -> Vec<(String, Vec<Line>)> {
        let prefix = wine_prefix().ok();
        let mut wine_lines = vec![wine_line(), bwrap_line()];
        wine_lines.push(sandbox_line(prefix.as_deref()));
        for (tool, package) in [("cabextract", "cabextract"), ("unzip", "unzip")] {
            wine_lines.push(setup_tool_line(tool, package));
        }
        wine_lines.push(gamescope_core_line());
        wine_lines.push(gstreamer_line());
        wine_lines.push(chrome_line());

        let bundled = system_dir().join("win/prefix");
        let (title, prefix_lines) = match &prefix {
            Some(prefix) => (
                format!("Wine prefix ({})", prefix.display()),
                prefix_lines(prefix, &bundled),
            ),
            None => ("Wine prefix".to_string(), vec![no_prefix_line()]),
        };
        vec![
            ("Wine".to_string(), wine_lines),
            (title, prefix_lines),
            ("Display".to_string(), display_lines()),
        ]
    }

    /// A tool's output, if it ran and said it succeeded.
    fn output_of(tool: &Path, args: &[&str]) -> Option<String> {
        let output = Command::new(tool)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// `(11, 17)` from `wine-11.17 (Staging)`.
    pub(super) fn parse_wine_version(text: &str) -> Option<(u32, u32)> {
        let (major, rest) = text.strip_prefix("wine-")?.split_once('.')?;
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        Some((major.parse().ok()?, rest[..end].parse().ok()?))
    }

    fn wine_line() -> Line {
        let Some(path) = find_tool("wine") else {
            return Line::new(Status::Missing, "wine", "not on PATH").fix(SETUP);
        };
        let Some(version) = output_of(&path, &["--version"]) else {
            return Line::new(
                Status::Missing,
                "wine",
                format!("{} does not run", path.display()),
            )
            .fix(SETUP);
        };
        let line = format!("{version} ({})", path.display());
        let wanted = wine::wanted_version();
        let old = parse_wine_version(&version)
            .zip(wanted.as_deref().and_then(parse_wine_version))
            .is_some_and(|(have, want)| have < want);
        if !old {
            return Line::new(Status::Ok, "wine", line);
        }
        let wanted = wanted.unwrap_or_default();
        let ours = dirs::data_dir().is_some_and(|dir| path.starts_with(dir.join("demarc/wine")));
        let fix = if ours {
            SETUP.to_string()
        } else {
            format!(
                "Update wine to {} or newer",
                wanted.trim_start_matches("wine-")
            )
        };
        Line::new(Status::Warn, "wine", format!("{line}, older than {wanted}")).fix(fix)
    }

    fn bwrap_line() -> Line {
        match find_tool("bwrap") {
            Some(path) => {
                let version = output_of(&path, &["--version"]).unwrap_or_default();
                Line::new(
                    Status::Ok,
                    "bubblewrap",
                    format!("{version} ({})", path.display()),
                )
            }
            None => Line::new(Status::Missing, "bubblewrap", "not on PATH").fix(SETUP),
        }
    }

    fn sandbox_line(prefix: Option<&Path>) -> Line {
        const NAME: &str = "Sandbox";
        let Some(prefix) = prefix.filter(|prefix| prefix.is_dir()) else {
            return Line::new(Status::Info, NAME, "not tested, there is no prefix yet");
        };
        if wine_sandbox::works(prefix) {
            return Line::new(Status::Ok, NAME, "works");
        }
        Line::new(
            Status::Warn,
            NAME,
            "bwrap cannot make one: Windows demos run one at a time, in the real prefix",
        )
        .fix("Install bubblewrap 0.11 or newer and allow unprivileged user namespaces")
    }

    fn setup_tool_line(tool: &'static str, package: &str) -> Line {
        match find_tool(tool) {
            Some(path) => Line::new(Status::Ok, tool, path.display().to_string()),
            None => Line::new(
                Status::Missing,
                tool,
                format!("not on PATH (package {package})"),
            )
            .fix(SETUP),
        }
    }

    fn gamescope_core_line() -> Line {
        const NAME: &str = "gamescope core";
        if libloader::has_core("gamescope") {
            Line::new(Status::Ok, NAME, "downloaded")
        } else {
            Line::new(Status::Info, NAME, "downloaded with the first Windows demo")
        }
    }

    /// `~/.local/share/demarc/gst` is where setup puts these on a Steam Deck.
    fn gstreamer_line() -> Line {
        const NAME: &str = "WMA music";
        let ours = dirs::data_dir().map(|dir| dir.join("demarc/gst"));
        if ours.is_some_and(|gst| {
            gst.join("libgstasf.so").is_file() && gst.join("libgstlibav.so").is_file()
        }) {
            return Line::new(Status::Ok, NAME, "gstreamer plugins installed");
        }
        const FIX: &str = "Install gst-plugins-ugly and gst-libav \
                           (Debian: gstreamer1.0-plugins-ugly gstreamer1.0-libav)";
        let Some(inspect) = find_tool("gst-inspect-1.0") else {
            return Line::new(Status::Warn, NAME, "gstreamer not found").fix(FIX);
        };
        let missing: Vec<_> = ["asfdemux", "avdec_wmav2"]
            .into_iter()
            .filter(|plugin| output_of(&inspect, &["--exists", plugin]).is_none())
            .collect();
        if missing.is_empty() {
            return Line::new(Status::Ok, NAME, "gstreamer plugins installed");
        }
        let detail = format!("no {}: demos with WMA music are silent", missing.join(", "));
        Line::new(Status::Warn, NAME, detail).fix(FIX)
    }

    fn chrome_line() -> Line {
        const NAME: &str = "Chrome";
        let found = CHROME_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .or_else(|| find_tool("chromium"));
        match found {
            Some(path) => Line::new(Status::Ok, NAME, path.display().to_string()),
            None => Line::new(Status::Warn, NAME, "not found: HTML releases cannot run")
                .fix("Install chromium"),
        }
    }

    fn no_prefix_line() -> Line {
        Line::new(Status::Missing, "Prefix", "not there").fix(SETUP)
    }

    /// Is `path` a real DLL, rather than the stub wine fills a prefix with? The
    /// stubs have the same names, so one existing proves nothing.
    pub(super) fn is_native(path: &Path) -> bool {
        const SIGNATURES: [&[u8]; 2] = [b"Wine builtin DLL", b"Wine placeholder DLL"];
        utils::read_header(path, 0x80).is_ok_and(|header| {
            !SIGNATURES
                .iter()
                .any(|sig| header.windows(sig.len()).any(|bytes| bytes == *sig))
        })
    }

    /// Does the hive `text` hold `value` under `key`?
    pub(super) fn has_reg_value(text: &str, key: &str, value: &str) -> bool {
        let header = format!("[{}]", key.to_ascii_lowercase());
        let value = value.to_ascii_lowercase();
        let mut lines = text.lines();
        lines.any(|line| line.to_ascii_lowercase().starts_with(&header))
            && lines
                .take_while(|line| !line.is_empty())
                .any(|line| line.to_ascii_lowercase().starts_with(&value))
    }

    /// What was found of a group of things setup puts in the prefix.
    #[derive(Clone, Copy, PartialEq)]
    enum Found {
        Yes,
        No,
        Outdated,
    }

    /// One line for a whole group, naming only what is wrong with it.
    fn group(name: &'static str, items: Vec<(String, Found)>) -> Line {
        let with = |wanted| {
            items
                .iter()
                .filter(|(_, found)| *found == wanted)
                .map(|(what, _)| what.as_str())
                .collect::<Vec<_>>()
        };
        let (missing, outdated) = (with(Found::No), with(Found::Outdated));
        let mut problems = vec![];
        if !missing.is_empty() {
            problems.push(format!("missing {}", missing.join(", ")));
        }
        if !outdated.is_empty() {
            problems.push(format!("outdated {}", outdated.join(", ")));
        }
        if problems.is_empty() {
            return Line::new(Status::Ok, name, format!("all {} there", items.len()));
        }
        let status = if missing.is_empty() {
            Status::Warn
        } else {
            Status::Missing
        };
        Line::new(status, name, problems.join("; ")).fix(SETUP)
    }

    /// `path` against the copy demarc carries in `bundled`.
    fn compare(path: &Path, bundled: &Path) -> Found {
        match (std::fs::read(path), std::fs::read(bundled)) {
            (Ok(have), Ok(want)) if have == want => Found::Yes,
            (Ok(_), _) => Found::Outdated,
            (Err(_), _) => Found::No,
        }
    }

    pub(super) fn prefix_lines(prefix: &Path, bundled: &Path) -> Vec<Line> {
        if !prefix.is_dir() {
            return vec![no_prefix_line()];
        }
        let drive_c = prefix.join("drive_c");
        let windows = drive_c.join("windows");
        let flag = |there: bool| if there { Found::Yes } else { Found::No };

        let fonts = FONTS
            .iter()
            .map(|(name, file)| {
                let there = windows.join("Fonts").join(file).is_file();
                (name.to_string(), flag(there))
            })
            .collect();
        let dlls = NATIVE_DLLS
            .iter()
            .map(|(name, files)| {
                let there = files.iter().all(|file| is_native(&drive_c.join(file)));
                (name.to_string(), flag(there))
            })
            .collect();
        let carried = |&(file, dir): &(&str, &str)| {
            let found = compare(&windows.join(dir).join(file), &bundled.join(file));
            (format!("{dir}/{file}"), found)
        };
        let mut extras: Vec<_> = EXTRAS.iter().map(carried).collect();
        let music = drive_c.join("users/Public/Music/Sample Music");
        extras.push(("Sample Music".to_string(), flag(music.is_dir())));

        let hive = |name: &str| {
            std::fs::read(prefix.join(name))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default()
        };
        let (user, system) = (hive("user.reg"), hive("system.reg"));
        let registry = REGISTRY
            .iter()
            .map(|(what, file, key, value)| {
                let text = if *file == "user.reg" { &user } else { &system };
                (what.to_string(), flag(has_reg_value(text, key, value)))
            })
            .collect();

        vec![
            Line::new(Status::Ok, "Prefix", "there"),
            group("Fonts", fonts),
            group("Native DLLs", dlls),
            group("Proxy DLLs", PROXIES.iter().map(carried).collect()),
            group("Codecs, extra files", extras),
            group("Registry", registry),
        ]
    }

    /// What `hyprctl getoption render:direct_scanout` says the value is.
    pub(super) fn parse_hypr_option(text: &str) -> Option<i64> {
        text.lines()
            .find_map(|line| line.trim().strip_prefix("int:")?.trim().parse().ok())
    }

    fn set_in_env(name: &str) -> bool {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    }

    fn display_lines() -> Vec<Line> {
        let wayland = set_in_env("WAYLAND_DISPLAY") || set_in_env("WAYLAND_SOCKET");
        let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        let on = if desktop.is_empty() {
            String::new()
        } else {
            format!(" ({desktop})")
        };
        const NAME: &str = "Session";
        if wayland {
            let session = Line::new(Status::Ok, NAME, format!("Wayland{on}"));
            return vec![session, direct_scanout_line(&desktop)];
        }
        if set_in_env("DISPLAY") {
            return vec![Line::new(Status::Ok, NAME, format!("X11{on}"))];
        }
        vec![Line::new(
            Status::Warn,
            NAME,
            "no display: only --headless can run here",
        )]
    }

    fn direct_scanout_line(desktop: &str) -> Line {
        const NAME: &str = "direct_scanout";
        if !set_in_env("HYPRLAND_INSTANCE_SIGNATURE") {
            let compositor = if desktop.is_empty() {
                "this compositor"
            } else {
                desktop
            };
            return Line::new(
                Status::Info,
                NAME,
                format!(
                    "cannot be checked on {compositor}; if fullscreen does not keep \
                     sync, start with `WAYLAND_DISPLAY= demarc`"
                ),
            );
        }
        let value = find_tool("hyprctl")
            .and_then(|tool| output_of(&tool, &["getoption", "render:direct_scanout"]))
            .as_deref()
            .and_then(parse_hypr_option);
        const FIX: &str = "Set `direct_scanout = 1` in the render section of your \
                           Hyprland config, or start with `WAYLAND_DISPLAY= demarc`";
        match value {
            Some(1) => Line::new(Status::Ok, NAME, "on"),
            Some(2) => Line::new(Status::Warn, NAME, "auto: only used for games").fix(FIX),
            Some(_) => Line::new(Status::Warn, NAME, "off: fullscreen will not keep sync").fix(FIX),
            None => Line::new(Status::Info, NAME, "hyprctl did not say"),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "tests/health_tests.rs"]
mod tests;
