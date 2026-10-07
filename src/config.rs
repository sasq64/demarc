use std::{collections::HashMap, path::PathBuf, sync::Arc};

use bevy::{color::Color, ecs::resource::Resource, render::extract_resource::ExtractResource};
use clap::{
    ColorChoice, Parser,
    builder::{Styles, styling},
};
use indexmap::IndexMap;
use newsys::{CbmSystem, SysOpts};
use regex::Regex;

use crate::{
    emu_file::{DemoId, EmuFile, Override},
    newsys::NewSys,
    post_process::{BorderMode, ScaleMode, ShaderEffect},
    system_dir::system_dir,
};

const CLAP_STYLES: Styles = Styles::styled()
    .header(
        styling::Style::new()
            .bold()
            .fg_color(Some(styling::Color::Ansi(styling::AnsiColor::Yellow))),
    )
    .usage(
        styling::Style::new()
            .bold()
            .fg_color(Some(styling::Color::Ansi(styling::AnsiColor::Yellow))),
    )
    .literal(
        styling::Style::new().fg_color(Some(styling::Color::Ansi(styling::AnsiColor::BrightRed))),
    )
    .placeholder(
        styling::Style::new().fg_color(Some(styling::Color::Ansi(styling::AnsiColor::Green))),
    );

#[derive(Parser, Debug, Resource, Clone)]
#[command(name = "demarc", version, styles = CLAP_STYLES, color = ColorChoice::Always,
    // clap 4 drops the version from the help header, so put it back by hand.
    help_template = "\
{name} {version}
{about-with-newline}
{usage-heading} {usage}

{all-args}{after-help}",
    about = "Demo scene emulator frontend for the command line",
    long_about = r#"
DEMARC

demarc is an emulator launcher/frontend with a focus on the (oldschool) demo scene.

Examples:
demarc edge_of_disgrace.zip
demarc --aga --shuffle AmigaDemos/
demarc --grid=3x3 gfx/*.prg
"#)]
pub struct Args {
    /// Path to the files to load, or an http(s):// URL to download and run
    pub files: Vec<PathBuf>,

    /// Demo database file to load, optionally gz/bz2 packed. A db can also be
    /// piped in on stdin.
    #[arg(long)]
    pub db: Option<String>,

    /// Treat disk images in same dir as separate files
    #[arg(long)]
    pub many: bool,

    /// Recursively look in directories for releases
    #[arg(long)]
    pub collect: bool,

    /// Limit to n demos. Useful with --sort + --shuffle
    #[arg(long, default_value_t = 0)]
    pub limit: usize,

    /// Skip first n demos. Mostly useful for grid testing
    #[arg(long, default_value_t = 0)]
    pub skip_count: usize,

    /// Start with the file-open selector showing and load nothing automatically.
    /// Any files/dirs given are still collected and become the selector's list.
    #[arg(short, long)]
    pub select: bool,

    /// Name of the file inside the release to start, skipping the systems' own
    /// file picking — the command-line spelling of an override's `boot` key,
    /// for a local archive or directory that has no db entry to write one for.
    /// Matched on the file name alone (ignoring case) anywhere inside the
    /// release, e.g. `--boot-file demo.exe`.
    #[arg(long)]
    pub boot_file: Option<String>,

    /// How to map emulator screen onto window: `stretch`, `fit`, `zoom`, or a
    /// scale factor like `2` or `2.5` (fractional allowed).
    #[arg(long, value_parser = parse_scale_mode, default_value = "fit")]
    pub scale: ScaleModeArg,

    /// How to fill the border outside the image.
    #[arg(long, value_enum, default_value_t = BorderModeArg::Black)]
    pub border: BorderModeArg,

    /// Shader used to render the emulator screen.
    #[arg(long, value_enum)]
    pub shader: Option<ShaderArg>,

    /// Path to a libretro `.slangp` shader preset to use instead of `--shader`,
    /// e.g. any preset from the slang-shaders repo. Takes precedence over
    /// `--shader`.
    #[arg(long)]
    pub slangp: Option<PathBuf>,

    /// Directory the shader dialog browses. Defaults to the first of
    /// `/usr/share/libretro/shaders` and `./shaders` that exists.
    #[arg(long)]
    pub shader_dir: Option<PathBuf>,

    /// Path to lua script used for music visualization
    #[arg(long)]
    pub lua: Option<PathBuf>,

    /// Drive demarc from a Luau script: it can send keys and hotkey commands,
    /// take screenshots and suspend itself for N frames.
    #[arg(long, value_name = "LUA")]
    pub remote_control: Option<PathBuf>,

    /// Only load db entries with a field matching this regex, e.g.
    /// `-I '(Demo|Intro)'`. Matched against each field of the db line on its
    /// own, so it can pick on any one of them but never spans two.
    /// Repeatable; all patterns must match.
    #[arg(short = 'I', long, value_parser = Regex::new)]
    pub include: Vec<Regex>,

    /// Exclude db entries with a field matching this regex, e.g.
    /// `-X 'category:.*Disk'`. Matched against each field of the db line on
    /// its own, so it can pick on any one of them but never spans two.
    /// Repeatable; a match on any pattern excludes.
    #[arg(short = 'X', long, value_parser = Regex::new)]
    pub exclude: Vec<Regex>,

    /// Shuffle the list of files into a random order. Applied after limiting.
    #[arg(long)]
    pub shuffle: bool,

    /// How to order the list of files. Applied before limiting.
    #[arg(long, value_enum)]
    pub sort: Option<SortArg>,

    /// When to show overlay info text
    #[arg(long, value_enum, default_value_t = InfoDisplay::OnMulti)]
    pub info: InfoDisplay,

    /// Amiga: Force AGA (A1200 with 8MB Fast RAM)
    #[arg(long)]
    pub aga: bool,

    /// Atari ST: Force STE
    #[arg(long)]
    pub ste: bool,

    /// Amiga: Force high specs (68030 + FPU)
    #[arg(long)]
    pub fast: bool,

    /// Amiga/Atari ST: add extra memory
    #[arg(long)]
    pub xmem: bool,

    /// Amiga: Unpack a single-disk AmigaDOS demo out of its ADF (or DMS) and
    /// boot it as a hard drive instead of as a floppy, which skips the drive
    /// seeks. Disks with no file system (trackloaded demos) are booted as
    /// floppies as usual.
    #[arg(long)]
    pub unadf: bool,

    /// C64: Always use Retro-replay to load
    /// Amiga: Turn off disk rotation emulation
    #[arg(long, verbatim_doc_comment)]
    pub fast_load: bool,

    /// Amiga,C64,Amstrad: Dont produce disk loading sound
    #[arg(long)]
    pub silent_drive: bool,

    /// Open windowed instead of full screen, optionally at a given size,
    /// e.g. --window=640x480
    #[arg(short, long, value_name = "WxH", num_args = 0..=1, require_equals = true,
          default_missing_value = "720x540", value_parser = parse_window)]
    pub window: Option<(u32, u32)>,

    /// Open no window and play no audio; render offscreen instead
    #[arg(long)]
    pub headless: bool,

    /// Max number of seconds to play a file before skipping
    #[arg(long)]
    pub max_time: Option<usize>,

    /// Benchmark: run emulation unthrottled (no vsync, audio dropped) for two
    /// seconds, print the number of frames stepped, then exit.
    #[arg(long)]
    pub speed_test: bool,

    /// Report whether Windows releases can be run here — wine, bwrap and the
    /// ~/.wine-demarc prefix — then exit. Without all three, .exe releases are
    /// left to whatever else can claim them.
    #[arg(long)]
    pub check_wine: bool,

    /// Run libretro cores in a separate process instead of a thread
    #[arg(long)]
    pub proc: bool,

    /// Max queued frames. Lower values = better input response
    #[arg(long, default_value_t = 2)]
    pub latency: u32,

    /// Extra options to add to libretro
    #[arg(short = 'x', long, value_delimiter = ',')]
    pub extra_options: Vec<String>,

    /// Render multiple emulators in a COLSxROWS grid, e.g. --grid=5x4
    #[arg(long, value_parser = parse_grid)]
    pub grid: Option<(u32, u32)>,

    /// Start with the first grid cell maximized, as if it was the only
    /// emulator running. Un-maximize (RightAlt+Enter) to see the whole grid.
    #[arg(long)]
    pub focus_first: bool,

    /// Load the next release into a second, hidden emulator and swap it in
    /// when it is ready.
    #[arg(long)]
    pub cross_fade: bool,

    /// Hold the cross fade back until the hidden emulator's screen activity
    /// rises above this threshold (0-1).
    #[arg(long)]
    pub cross_fade_activity: Option<f32>,

    /// Open a second window showing the cross fade emulator, and fade it in
    /// only when asked to (RightAlt+Shift+O). Implies `--cross-fade`.
    #[arg(long)]
    pub dj_mode: bool,

    /// Background clear color as a hex string, e.g. `#003` or `000080`.
    #[arg(long, value_parser = parse_color, default_value = "000000")]
    pub clear_color: Color,

    /// C64: Add ram expansion unit (16MB)
    #[arg(long)]
    pub reu: bool,

    /// ILBM: Animate colour-cycling (CRNG) ranges. Off by default.
    #[arg(short = 'C', long)]
    pub color_cycle: bool,

    /// Commodore variant (C64 and C16 well supported)
    #[arg(long, value_enum, default_value_t = CbmSystemArg::C64)]
    pub cbm_variant: CbmSystemArg,

    /// Don't silence libretro cores' stdout/stderr (for debugging)
    #[arg(long)]
    pub no_silence: bool,

    /// Skip bad files, loop playlist and detect idle demos
    #[arg(long)]
    pub tv_mode: bool,

    /// Skip demo after still screen and no audio
    #[arg(long, default_value_t = 0)]
    pub idle_timeout: i32,

    /// Delay until info is shown for new file
    #[arg(long, default_value_t = 4)]
    pub info_delay: u64,

    /// Duration of info showing for new file
    #[arg(long, default_value_t = 8)]
    pub info_duration: u64,

    /// Turn the CRT filter off when the image is magnified less than this
    /// factor, e.g. `2` disables it whenever a 320x240 screen is shown smaller
    /// than 640x480. `0` never disables it.
    #[arg(long, default_value_t = 1.0)]
    pub crt_limit: f32,

    /// Filter the image with the DREZ downsampler instead of the CRT/LCD
    /// shader when the image is magnified less than this factor. `1` (the
    /// default) switches to it whenever the image is shown *smaller* than its
    /// source resolution.
    #[arg(long, default_value_t = 1.0)]
    pub downsample: f32,

    // Max threads in bevy thread pool. Probably don't touch this.
    #[arg(long, default_value_t = 4)]
    pub max_threads: u32,
}

/// Parse a hex color string like `#003`, `#000080`, or `000080` into a [`Color`].
fn parse_color(s: &str) -> Result<bevy::color::Color, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let expand = |c: char| -> String { format!("{c}{c}") };
    let (r, g, b) = match hex.len() {
        3 => {
            let mut chars = hex.chars();
            (
                expand(chars.next().unwrap()),
                expand(chars.next().unwrap()),
                expand(chars.next().unwrap()),
            )
        }
        6 => (hex[0..2].into(), hex[2..4].into(), hex[4..6].into()),
        _ => {
            return Err(format!(
                "expected 3 or 6 hex digits, e.g. 000080 (got `{s}`)"
            ));
        }
    };
    let parse =
        |c: String| u8::from_str_radix(&c, 16).map_err(|_| format!("invalid hex color `{s}`"));
    Ok(bevy::color::Color::srgb_u8(parse(r)?, parse(g)?, parse(b)?))
}

/// Parse an `AxB` pair of non-zero numbers like `5x4`.
fn parse_pair(s: &str, form: &str) -> Result<(u32, u32), String> {
    let (a, b) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected {form} (got `{s}`)"))?;
    let num = |v: &str| match v.trim().parse() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!("invalid number `{v}` in `{s}`, expected {form}")),
    };
    Ok((num(a)?, num(b)?))
}

fn parse_grid(s: &str) -> Result<(u32, u32), String> {
    parse_pair(s, "COLSxROWS, e.g. 5x4")
}

fn parse_window(s: &str) -> Result<(u32, u32), String> {
    parse_pair(s, "WIDTHxHEIGHT, e.g. 640x480")
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum SortArg {
    /// Shuffle into a random order.
    Random,
    /// Best pouet.net rank first; unranked entries last.
    Rank,
    // Newest first
    Date,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum InfoDisplay {
    /// Always show demo info on start
    Always,
    /// Dont show demo info on start
    Never,
    /// Show demo info on start with multiple files
    OnMulti,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ScaleModeArg {
    /// Fill the window, distorting the aspect ratio.
    Stretch,
    /// Preserve aspect ratio, adding letterbox/pillarbox bars.
    Fit,
    /// Preserve aspect ratio, cropping top/bottom or left/right to fill.
    Zoom,
    /// Scale the source by a fixed factor, centred. Whole numbers keep pixels
    /// integer-sized; fractional factors (e.g. 2.5) are applied exactly.
    Fixed(f32),
}

/// Parse a `--scale` value: one of the named modes (`stretch`, `fit`, `zoom`)
/// or a positive scale factor like `2`, `2x`, or `2.5`.
fn parse_scale_mode(s: &str) -> Result<ScaleModeArg, String> {
    match s.to_ascii_lowercase().as_str() {
        "stretch" => Ok(ScaleModeArg::Stretch),
        "fit" => Ok(ScaleModeArg::Fit),
        "zoom" => Ok(ScaleModeArg::Zoom),
        other => {
            // Accept an optional trailing `x`, e.g. `2x` or `2.5x`.
            let num = other.strip_suffix('x').unwrap_or(other);
            match num.parse::<f32>() {
                Ok(n) if n.is_finite() && n > 0.0 => Ok(ScaleModeArg::Fixed(n)),
                _ => Err(format!(
                    "expected stretch, fit, zoom, or a positive scale factor \
                     like 2 or 2.5 (got `{s}`)"
                )),
            }
        }
    }
}

/// `--cbm-variant`, as clap sees it. The systems have their own copy (they do
/// not know what a command line is), and this converts into it.
#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum CbmSystemArg {
    /// Default Commodore C64
    C64,
    /// Commodore 128
    C128,
    /// C64 DTV Stick
    Dtv,
    /// C16/Plus4
    C16,
    /// VIC 20
    VIC20,
}

impl From<CbmSystemArg> for CbmSystem {
    fn from(s: CbmSystemArg) -> Self {
        match s {
            CbmSystemArg::C64 => CbmSystem::C64,
            CbmSystemArg::C128 => CbmSystem::C128,
            CbmSystemArg::Dtv => CbmSystem::Dtv,
            CbmSystemArg::C16 => CbmSystem::C16,
            CbmSystemArg::VIC20 => CbmSystem::VIC20,
        }
    }
}

/// Meta applied to every file loaded.
#[derive(Resource, Default, Clone)]
pub struct GlobalSettings(pub HashMap<&'static str, String>);

/// The slice of the command line the systems care about.
impl From<&Args> for SysOpts {
    fn from(a: &Args) -> Self {
        SysOpts {
            aga: a.aga,
            cbm_variant: a.cbm_variant.into(),
            extra_options: a.extra_options.clone(),
            fast: a.fast,
            grid: a.grid,
            latency: a.latency,
            lua: a.lua.clone(),
            proc: a.proc,
            reu: a.reu,
            silent_drive: a.silent_drive,
            unadf: a.unadf,
            xmem: a.xmem,
        }
    }
}

impl From<ScaleModeArg> for ScaleMode {
    fn from(s: ScaleModeArg) -> Self {
        match s {
            ScaleModeArg::Stretch => ScaleMode::Stretch,
            ScaleModeArg::Fit => ScaleMode::Fit,
            ScaleModeArg::Zoom => ScaleMode::Zoom,
            ScaleModeArg::Fixed(n) => ScaleMode::Fixed(n),
        }
    }
}

/// One of the bundled post-process shaders. Picked on the command line with
/// `--shader` and, at runtime, from the settings dialog -- which is why it is
/// `Reflect` (the dialog reads its variant list off the type) and `PartialEq`
/// (only a shader that actually changed re-points the render world at a new
/// preset, so an unrelated Apply cannot throw away a `--slangp`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, bevy::reflect::Reflect, clap::ValueEnum)]
pub enum ShaderArg {
    /// Timothy Lottes CRT shader — scanlines/shadow mask, for CRT-era systems.
    #[default]
    Lottes,
    /// Single-pass WGSL port of the Lottes CRT shader
    LottesSimple,
    /// cgwg dot-matrix LCD grid shader, for handheld LCD systems.
    Lcd,
    /// Lightweight single-pass LCD grid shader.
    LcdSimple,
    /// No post-process effect — render the raw emulator screen.
    None,
}

impl ShaderArg {
    /// Path of the shader, relative to the `system` asset directory: either a
    /// RetroArch libretro `.slangp` preset bundled under `shaders/slangp/`
    /// (see `system/shaders/slangp/`) and run through librashader, or a plain
    /// `.wgsl` shader loaded as a Bevy asset and run as a single pass.
    pub fn path(self) -> &'static str {
        match self {
            ShaderArg::Lottes => "shaders/slangp/crt/crt-lottes.slangp",
            ShaderArg::LottesSimple => "shaders/lottes.wgsl",
            ShaderArg::Lcd => "shaders/slangp/handheld/lcd-grid-v2.slangp",
            //ShaderArg::LcdSimple => "shaders/slangp/handheld/zfast-lcd.slangp",
            ShaderArg::LcdSimple => "shaders/lcd.wgsl",
            // `None` starts with the effect disabled (see `crt_effect`); the
            // path only matters if it's toggled on, so reuse the stock
            // passthrough preset.
            ShaderArg::None => "shaders/slangp/stock.slangp",
        }
    }

    /// Which post-process backend this shader selects, with its path resolved:
    /// a `.wgsl` name picks the single-pass backend and stays an asset path
    /// (the Bevy asset root *is* the `system` dir), anything else a librashader
    /// preset, which librashader opens itself and so needs the full path.
    pub fn effect(self) -> ShaderEffect {
        let path = self.path();
        if path.ends_with(".wgsl") {
            ShaderEffect::Wgsl(path.into())
        } else {
            ShaderEffect::Slangp(system_dir().join(path))
        }
    }
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum BorderModeArg {
    /// Stretch the edge pixels outward into the border.
    Stretch,
    /// Fill the border with background color.
    Black,
}

impl From<BorderModeArg> for BorderMode {
    fn from(b: BorderModeArg) -> Self {
        match b {
            BorderModeArg::Stretch => BorderMode::Stretch,
            BorderModeArg::Black => BorderMode::Black,
        }
    }
}

/// The subset of settings the render world needs. This is the only resource
/// that is [`ExtractResource`], so it is the only thing Bevy clones into the
/// render world each frame — keeping the large [`AppSettings`] off the
/// per-frame extract path. Mutated directly by hotkeys in the main world (see
/// `handle_cmd`); the extract copies the fresh values across.
#[derive(Resource, Default, Clone, ExtractResource)]
pub struct RenderSettings {
    pub border_mode: BorderMode,
    pub scale_mode: ScaleMode,
    pub crt_effect: bool,
}

#[derive(Resource, Default)]
pub struct AppSettings {
    /// Shared so that a load can run the whole of
    /// [`NewSys::load_prepared`] on a job thread; see
    /// [`Emulator::update_load`](crate::emulator::Emulator::update_load).
    pub system: Arc<NewSys>,
    pub show_info: bool,
    /// Every loaded database by name, in the order it was given on the command
    /// line — a db file, a db on stdin, or the plain files and directories
    /// collected under `Files`.
    ///
    /// Each list is leaked at startup: the entries hold `&'static str` slices
    /// into the leaked db text (see [`crate::files`]) and are read for the
    /// length of the run, so a list is `'static` too and can be handed out
    /// without borrowing this resource.
    pub files: IndexMap<String, &'static [EmuFile]>,
    pub current_game: isize,
    pub current_emu: usize,
    pub maximized: bool,
    pub all_emus: bool,
    pub select_box_drawn_at: f64,
    pub hotkey_pressed_at: f32,
    pub mouse_index: Option<usize>,
    pub speed_test: bool,
    pub tv_mode: bool,
    pub idle_timeout: i32,
    pub info_delay: u64,
    pub info_duration: u64,
    /// Minimum magnification (on-screen pixels per source pixel) the CRT filter
    /// needs to stay on. Below it the effect is bypassed even when
    /// [`RenderSettings::crt_effect`] is set, because the scanlines/phosphor
    /// mask alias into mud at low magnification — most visibly in grid mode,
    /// where each cell is a fraction of the window. `0` disables the check.
    ///
    /// Applied per emulator view (see `post_process::geometry::compute_uniform`), so the
    /// same core can render without the filter in a small grid cell and with it
    /// once maximized.
    pub crt_limit: f32,

    /// Per-release fixups read from `overrides.toml` at startup, keyed on the
    /// db id of the release each one is for — see [`crate::overrides`].
    /// Empty when there is no such file, which is the normal case.
    pub overrides: HashMap<DemoId, Override>,

    /// `--boot-file`: the program to start, for every release loaded this run.
    /// An overrides file can only name one per db id, so this is how a
    /// local archive or directory — which has no id — gets the same treatment.
    pub boot_file: Option<&'static str>,
}

impl AppSettings {
    /// The db to work in when nothing says which one an index or an id belongs
    /// to, and the list `current_game` walks.
    ///
    /// NOTE: ids are only unique within one db, so entries have to carry the db
    /// they came from before a lookup can pick the right one out of several.
    pub fn default_db(&self) -> &'static [EmuFile] {
        self.files.values().next().copied().unwrap_or(&[])
    }

    pub fn override_for_file(&self, emu_file: &EmuFile) -> Option<Override> {
        let from_file = emu_file.demo_id().and_then(|id| self.overrides.get(&id));
        if from_file.is_none() && self.boot_file.is_none() {
            return None;
        }
        let mut over = from_file.cloned().unwrap_or_default();
        if self.boot_file.is_some() {
            over.boot_file = self.boot_file;
        }
        Some(over)
    }
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod tests;
