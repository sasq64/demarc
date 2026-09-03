use std::{collections::HashMap, path::PathBuf};

use bevy::{color::Color, ecs::resource::Resource, render::extract_resource::ExtractResource};
use clap::{
    ColorChoice, Parser,
    builder::{Styles, styling},
};
use regex::Regex;

use crate::{
    commands::FilePickerSource,
    emu_file::{EmuFile, Override},
    newsys::NewSys,
    post_process::{BorderMode, ScaleMode},
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

    /// Path to lua script used for music visualization
    #[arg(long)]
    pub lua: Option<PathBuf>,

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

    /// C64: Always use Retro-replay to load
    /// Amiga: Turn off disk rotation emulation
    #[arg(long, verbatim_doc_comment)]
    pub fast_load: bool,

    /// Amiga,C64,Amstrad: Dont produce disk loading sound
    #[arg(long)]
    pub silent_drive: bool,

    /// Open windowed instead of full screen
    #[arg(short, long)]
    pub window: bool,

    /// Max number of seconds to play a file before skipping
    #[arg(long)]
    pub max_time: Option<usize>,

    /// Benchmark: run emulation unthrottled (no vsync, audio dropped) for two
    /// seconds, print the number of frames stepped, then exit.
    #[arg(long)]
    pub speed_test: bool,

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

    /// Cross-fade between demos.
    /// Optionally takes the length of the fade in seconds, e.g. `--cross-fade=4`.
    /// Not usable with `--grid`
    #[arg(
        long,
        value_name = "SECS",
        num_args = 0..=1,
        // Only `--cross-fade=SECS` gives a value, so a bare `--cross-fade`
        // can't swallow the file that follows it on the command line.
        require_equals = true,
        default_missing_value = "2",
        conflicts_with = "grid"
    )]
    pub cross_fade: Option<f32>,

    /// Seconds a cross-faded release runs off screen before it starts fading
    /// in, so the fade doesn't start on its loading screen.
    #[arg(
        long,
        value_name = "SECS",
        default_value_t = 2.0,
        requires = "cross_fade"
    )]
    pub cross_fade_delay: f32,

    /// Hold each cross-fade until the release coming in is making
    /// sound, instead of fading it in a fixed time after it booted.
    /// Implies `--silent-drive`, so a clicking disk
    /// drive can't pass for the release having started.
    #[arg(long, requires = "cross_fade")]
    pub cross_wait_sound: bool,

    /// Background clear color as a hex string, e.g. `#003` or `000080`.
    #[arg(long, value_parser = parse_color, default_value = "000033")]
    pub clear_color: Color,

    /// C64: Add ram expansion unit (16MB)
    #[arg(long)]
    pub reu: bool,

    /// ILBM: Animate colour-cycling (CRNG) ranges. Off by default.
    #[arg(short = 'C', long)]
    pub color_cycle: bool,

    /// Commodore variant (C64 and C16 well supported)
    #[arg(long, value_enum, default_value_t = CbmSystem::C64)]
    pub cbm_variant: CbmSystem,

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

impl Args {
    /// Turn on the flags other flags imply. Called once, right after parsing —
    /// clap's `requires`/`conflicts_with` can say which combinations are legal
    /// but not fill one in from another.
    pub fn apply_implications(&mut self) {
        // `--cross-wait-sound` waits for the release coming in to make a sound
        // before it fades it in, and an Amiga drive clicking its way through a
        // loading screen is a sound — the very part of the boot the wait exists
        // to sit through. Silence the drive so only the release itself can end
        // the wait.
        if self.cross_wait_sound {
            self.silent_drive = true;
        }
    }
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

/// Parse a `COLSxROWS` grid specifier like `5x4` into `(cols, rows)`.
fn parse_grid(s: &str) -> Result<(u32, u32), String> {
    let (cols, rows) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected COLSxROWS, e.g. 5x4 (got `{s}`)"))?;
    let cols: u32 = cols
        .trim()
        .parse()
        .map_err(|_| format!("invalid column count `{cols}`"))?;
    let rows: u32 = rows
        .trim()
        .parse()
        .map_err(|_| format!("invalid row count `{rows}`"))?;
    if cols == 0 || rows == 0 {
        return Err("grid dimensions must be at least 1".into());
    }
    Ok((cols, rows))
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

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum CbmSystem {
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

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum ShaderArg {
    /// Timothy Lottes CRT shader — scanlines/shadow mask, for CRT-era systems.
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
/// render world each frame — keeping the large [`AppSettings`] (and its `files`
/// vec) off the per-frame extract path. Mutated directly by hotkeys in the main
/// world (see `handle_cmd`); the extract copies the fresh values across.
#[derive(Resource, Default, Clone, ExtractResource)]
pub struct RenderSettings {
    pub border_mode: BorderMode,
    pub scale_mode: ScaleMode,
    pub crt_effect: bool,
}

/// Progress of a `--cross-fade` transition between the two emulators
/// [`RetroPlugin`](crate::frontend::RetroPlugin) spawns for it.
///
/// The release on screen is never reloaded in place: its request to advance is
/// handed to the other emulator, which boots the next release off screen and
/// then fades in over it. Everything the fade needs is here; `AppSettings::
/// current_emu` still names the emulator that is (fully) on screen.
/// How long a `--cross-wait-sound` hold ignores whatever the incoming release
/// is playing. Cores routinely emit a burst of static, a click or the tail of
/// the previous buffer as they come up, and that is not the release starting.
pub(crate) const SOUND_GRACE_SECS: f64 = 0.5;

/// How long a `--cross-wait-sound` hold waits for a sound before fading in
/// regardless. Plenty of releases are silent — and a backend that can't report
/// its audio always reads as audible, so this only bites on a core that really
/// does stay quiet — but without a cap one of them would park the playlist for
/// good.
pub(crate) const SOUND_TIMEOUT_SECS: f64 = 30.0;

/// A `--cross-wait-sound` hold: the next release is loaded and running off
/// screen, but the fade over the one on show waits until it is audible.
///
/// The hold does not replace `--cross-fade-delay`, it moves it: the delay is
/// counted from the moment the release is first heard rather than from the
/// moment it started loading, so it goes on being the breathing space between
/// "this has got going" and the fade — the sound is just a far better mark for
/// that than a fixed wait after the boot. With `--cross-fade-delay 0` the fade
/// starts on the sound itself.
#[derive(Copy, Clone, Debug)]
pub struct SoundWait {
    /// Sound before this is ignored — see [`SOUND_GRACE_SECS`].
    pub not_before: f64,
    /// When the hold gives up waiting for a sound that isn't coming.
    pub deadline: f64,
}

impl SoundWait {
    /// Start a hold on a release that began running at `now`.
    pub fn starting_at(now: f64) -> Self {
        Self {
            not_before: now + SOUND_GRACE_SECS,
            deadline: now + SOUND_TIMEOUT_SECS,
        }
    }

    /// Whether the hold is over at `now`, given whether the incoming emulator
    /// is silent right this frame. `--cross-fade-delay` is counted from here.
    pub fn is_over(&self, now: f64, silent: bool) -> bool {
        now >= self.deadline || (now >= self.not_before && !silent)
    }
}

#[derive(Default)]
pub struct FadeState {
    /// The emulator the next release was loaded into, from the moment the load
    /// is handed to it until its fade has finished. `None` when nothing is on
    /// its way in.
    pub incoming: Option<usize>,
    /// When the fade starts. `f64::MAX` while the release is still loading —
    /// the fade only begins once it is actually running (plus
    /// [`AppSettings::cross_fade_delay`]).
    pub start: f64,
    /// How far the incoming emulator has faded in, `0..=1`.
    pub alpha: f32,
    /// `--cross-wait-sound`: the hold keeping [`Self::start`] at `f64::MAX`
    /// until the incoming release makes a sound. `None` whenever the fade runs
    /// on the fixed `--cross-fade-delay` instead, which is the default.
    pub wait_sound: Option<SoundWait>,
    /// The info text announcing the incoming release, held until the fade
    /// actually starts. Only used under a [`Self::wait_sound`] hold, where the
    /// load has no idea yet *when* that will be — everywhere else the text is
    /// written with a delay that says it.
    pub pending_info: Option<String>,
}

impl FadeState {
    /// Drop the fade in flight: nothing is on its way in any more, so the
    /// emulator on screen keeps the window and the next request can hand this
    /// one another release. [`Self::start`] is left alone — it means nothing
    /// with no incoming emulator, and is dated afresh by whatever starts the
    /// next fade.
    pub fn clear(&mut self) {
        self.incoming = None;
        self.alpha = 0.0;
        self.wait_sound = None;
        self.pending_info = None;
    }
}

#[derive(Resource, Default)]
pub struct AppSettings {
    pub system: NewSys,
    pub show_info: bool,
    pub files: Vec<EmuFile>,
    pub current_game: isize,
    pub current_emu: usize,
    pub maximized: bool,
    pub all_emus: bool,
    pub last_draw: f64,
    /// The file picker's search index, built lazily from `files` on first open
    /// and reused (cheap `Arc` clone) on every open after that — building the
    /// trigram index over the whole list is the picker's expensive step.
    pub file_source: Option<FilePickerSource>,
    pub hotkey_pressed: f32,
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
    /// Applied per emulator view (see `post_process::compute_uniform`), so the
    /// same core can render without the filter in a small grid cell and with it
    /// once maximized.
    pub crt_limit: f32,

    /// Per-release fixups read from `overrides.toml` at startup, keyed on the
    /// demozoo id of the release each one is for — see [`crate::overrides`].
    /// Empty when there is no such file, which is the normal case.
    pub demozoo_overrides: HashMap<usize, Override>,

    /// `--boot-file`: the program to start, for every release loaded this run.
    /// An overrides file can only name one per demozoo id, so this is how a
    /// local archive or directory — which has no id — gets the same treatment.
    pub boot_file: Option<&'static str>,

    /// `--cross-fade`: how long a fade between two releases takes, in seconds.
    /// `None` (the default) plays one release at a time and cuts between them.
    pub cross_fade: Option<f32>,
    /// `--cross-fade-delay`: seconds between the next release starting off
    /// screen and its fade beginning. Under [`Self::cross_wait_sound`] it is
    /// counted from the moment that release is first heard instead.
    pub cross_fade_delay: f64,
    /// `--cross-wait-sound`: hold each fade until the incoming release is
    /// audible (see [`SoundWait`]).
    pub cross_wait_sound: bool,
    /// The cross-fade in progress, if any. Only ever non-default when
    /// `cross_fade` is set.
    pub fade: FadeState,
}

impl AppSettings {
    /// How opaque emulator `i`'s view is drawn, `0..=1`.
    ///
    /// Always `1` unless a cross-fade is running, where the emulator on screen
    /// stays opaque and the incoming one is alpha-blended over it — which
    /// composites to exactly `outgoing * (1 - a) + incoming * a`, with no
    /// dependence on the clear colour showing through. Anything else (the
    /// emulator parked in the background between fades) is fully transparent
    /// and skipped by the render pass.
    pub fn view_alpha(&self, i: usize) -> f32 {
        if self.cross_fade.is_none() {
            return 1.0;
        }
        if i == self.current_emu {
            1.0
        } else if self.fade.incoming == Some(i) {
            self.fade.alpha
        } else {
            0.0
        }
    }

    /// Gain emulator `i`'s audio is mixed at, `0..=1`.
    ///
    /// Unlike the picture, both sides of a cross-fade are attenuated: the two
    /// emulators have a stream each and the device sums them, so the outgoing
    /// one has to ramp *down* as the incoming ramps up. The ramps are
    /// equal-power (`sqrt`) rather than linear, because two uncorrelated
    /// signals add in power, not amplitude — linear ramps would dip audibly at
    /// the half-way point.
    pub fn audio_gain(&self, i: usize) -> f32 {
        if self.cross_fade.is_none() {
            return 1.0;
        }
        let alpha = self.fade.incoming.map_or(0.0, |_| self.fade.alpha);
        if i == self.current_emu {
            (1.0 - alpha).max(0.0).sqrt()
        } else if self.fade.incoming == Some(i) {
            alpha.sqrt()
        } else {
            0.0
        }
    }

    /// The override to load `file` with: whatever `overrides.toml` said about
    /// the release it is, with `--boot-file` written over the top.
    ///
    /// Entries are matched on the `id` field a db line carries, so the file
    /// only ever finds anything for a release loaded out of a db; a file named
    /// on the command line has no id. The ids are demozoo's, so a db from
    /// somewhere else can in principle collide with one — the overrides exist
    /// for demos demarc gets wrong, and are written against the demozoo db they
    /// were tried on. `--boot-file` is not keyed on anything and so applies to
    /// every release loaded, which is what makes it usable on a local archive
    /// or directory; being asked for by hand, it also beats the file.
    pub fn override_for(&self, file: &EmuFile) -> Option<Override> {
        let from_file = if self.demozoo_overrides.is_empty() {
            None
        } else {
            file.get_meta("id")
                .parse::<usize>()
                .ok()
                .and_then(|id| self.demozoo_overrides.get(&id))
        };
        match (from_file, self.boot_file) {
            (over, None) => over.cloned(),
            (over, boot) => Some(Override {
                boot_file: boot,
                ..over.cloned().unwrap_or_default()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two emulators of a cross-fade need the whole window each, which is
    /// exactly what a grid can't give them.
    #[test]
    fn cross_fade_and_grid_are_mutually_exclusive() {
        assert!(Args::try_parse_from(["demarc", "--cross-fade", "--grid=2x2"]).is_err());
        assert!(Args::try_parse_from(["demarc", "--cross-fade"]).is_ok());
        assert!(Args::try_parse_from(["demarc", "--grid=2x2"]).is_ok());
    }

    /// A bare `--cross-fade` takes the default length; `--cross-fade=SECS`
    /// sets it. The value has to be attached with `=`, or the flag would eat
    /// the file that follows it.
    #[test]
    fn cross_fade_length_is_optional() {
        let args = Args::try_parse_from(["demarc"]).unwrap();
        assert_eq!(args.cross_fade, None);

        let args = Args::try_parse_from(["demarc", "--cross-fade"]).unwrap();
        assert_eq!(args.cross_fade, Some(2.0));

        let args = Args::try_parse_from(["demarc", "--cross-fade=4.5"]).unwrap();
        assert_eq!(args.cross_fade, Some(4.5));

        let args = Args::try_parse_from(["demarc", "--cross-fade", "demo.adf"]).unwrap();
        assert_eq!(args.cross_fade, Some(2.0));
        assert_eq!(args.files, vec![PathBuf::from("demo.adf")]);
    }

    /// `--cross-wait-sound` needs a fade to hold back, and silences the drive
    /// so a loading Amiga can't end the wait by clicking.
    #[test]
    fn waiting_for_sound_needs_a_fade_and_implies_a_silent_drive() {
        assert!(Args::try_parse_from(["demarc", "--cross-wait-sound"]).is_err());

        let mut args =
            Args::try_parse_from(["demarc", "--cross-fade", "--cross-wait-sound"]).unwrap();
        assert!(!args.silent_drive, "not until the implications are applied");
        args.apply_implications();
        assert!(args.silent_drive);
    }

    /// The hold ends on the first sound after the grace period, or on its own
    /// deadline — whichever comes first.
    #[test]
    fn a_sound_wait_ends_on_sound_or_on_its_deadline() {
        let wait = SoundWait::starting_at(100.0);

        // Startup static, inside the grace period: ignored.
        assert!(!wait.is_over(100.0, false));
        assert!(!wait.is_over(100.0 + SOUND_GRACE_SECS - 0.01, false));
        // The same sound once the grace period is up ends the hold.
        assert!(wait.is_over(100.0 + SOUND_GRACE_SECS, false));
        // Silence holds it — until the deadline gives up waiting.
        assert!(!wait.is_over(100.0 + SOUND_TIMEOUT_SECS - 0.01, true));
        assert!(wait.is_over(100.0 + SOUND_TIMEOUT_SECS, true));
    }

    /// Settings mid-fade: emulator 0 on screen, emulator 1 fading in over it.
    fn fading(alpha: f32) -> AppSettings {
        AppSettings {
            cross_fade: Some(2.0),
            current_emu: 0,
            fade: FadeState {
                incoming: Some(1),
                start: 0.0,
                alpha,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// The outgoing view stays opaque and the incoming one is blended over it,
    /// so the two alphas are `1` and the fade position — not `1 - a` and `a`.
    #[test]
    fn the_incoming_view_alone_fades_in() {
        let s = fading(0.25);
        assert_eq!(s.view_alpha(0), 1.0);
        assert_eq!(s.view_alpha(1), 0.25);

        // Between fades the emulator parked in the background is invisible,
        // which is what makes the render pass skip it entirely.
        let mut s = fading(0.0);
        s.fade.incoming = None;
        assert_eq!(s.view_alpha(0), 1.0);
        assert_eq!(s.view_alpha(1), 0.0);
    }

    /// Both audio streams are attenuated, on equal-power ramps that hold the
    /// summed level roughly constant across the fade.
    #[test]
    fn both_sides_of_the_audio_fade() {
        let s = fading(0.0);
        assert_eq!(s.audio_gain(0), 1.0);
        assert_eq!(s.audio_gain(1), 0.0);

        let s = fading(1.0);
        assert_eq!(s.audio_gain(0), 0.0);
        assert_eq!(s.audio_gain(1), 1.0);

        // Half way, equal-power means both sides sit at ~0.707, not 0.5.
        let s = fading(0.5);
        assert!((s.audio_gain(0) - 0.5f32.sqrt()).abs() < 1e-6);
        assert!((s.audio_gain(1) - 0.5f32.sqrt()).abs() < 1e-6);
        let power = s.audio_gain(0).powi(2) + s.audio_gain(1).powi(2);
        assert!((power - 1.0).abs() < 1e-6);
    }

    /// Without `--cross-fade` nothing is ever attenuated or blended, whatever
    /// the (unused) fade state happens to say.
    #[test]
    fn no_cross_fade_leaves_every_view_opaque() {
        let mut s = fading(0.5);
        s.cross_fade = None;
        for i in 0..4 {
            assert_eq!(s.view_alpha(i), 1.0);
            assert_eq!(s.audio_gain(i), 1.0);
        }
    }
}
