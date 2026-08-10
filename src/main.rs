// Needed for bevy systems
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use std::path::{Path, PathBuf};

use bevy::render::extract_resource::ExtractResource;
use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{prelude::*, window::PresentMode};
use clap::builder::styling::{AnsiColor, Style};
use clap::builder::{Styles, styling};
use clap::{ColorChoice, Parser};
use regex::Regex;

#[allow(warnings)]
mod libretro;

mod audio;
mod cbmconvert;
mod commands;
mod emulator;
mod fetch;
mod files;
#[cfg(feature = "flash")]
mod flash_emu;
mod frontend;
mod fuzzy_list;
mod hud;
mod ilbm;
mod image_emu;
mod libloader;
mod load_error;
mod media_keys;
mod post_process;
#[cfg(feature = "profile")]
mod profiling;
mod retro_emu;
mod screensaver;
mod speed_test;
mod systems;
mod text_input;
mod utils;

use commands::CommandPlugin;
use frontend::{RetroPlugin, system_dir};
use fuzzy_list::{FuzzyListPlugin, IndexedSource};
use hud::HudPlugin;
use post_process::{BorderMode, PostProcessPlugin, ScaleMode, ShaderPath};
use screensaver::ScreenSaverPlugin;
use speed_test::SpeedTestPlugin;
use text_input::TextInputPlugin;
#[cfg(not(feature = "profile"))]
use tracing_subscriber::EnvFilter;

use crate::files::{DbFilter, EmuFile, collect_db, collect_db_stdin, collect_file, collect_files};

const CLAP_STYLES: Styles = Styles::styled()
    .header(
        Style::new()
            .bold()
            .fg_color(Some(styling::Color::Ansi(AnsiColor::Yellow))),
    )
    .usage(
        Style::new()
            .bold()
            .fg_color(Some(styling::Color::Ansi(AnsiColor::Yellow))),
    )
    .literal(Style::new().fg_color(Some(styling::Color::Ansi(AnsiColor::BrightRed))))
    .placeholder(Style::new().fg_color(Some(styling::Color::Ansi(AnsiColor::Green))));

#[derive(Parser, Debug, Resource, Clone)]
#[command(name = "demarc", styles = CLAP_STYLES, color = ColorChoice::Always, 
    about = "Demo scene emulator frontend for the command line",
    long_about = r#"
DEMARC

demarc is an emulator launcher/frontend with a focus on the (oldschool) demo scene.

Examples:
demarc edge_of_disgrace.zip
demarc --aga --shuffle AmigaDemos/
demarc --grid=3x3 gfx/*.prg
"#)]
struct Args {
    /// Path to the files to load, or an http(s):// URL to download and run
    files: Vec<PathBuf>,

    /// Demo database file to load, optionally gz/bz2 packed. A db can also be
    /// piped in on stdin.
    #[arg(long)]
    db: Option<String>,

    /// Treat disk images in same dir as separate files
    #[arg(long)]
    many: bool,

    /// Start with the file-open selector showing and load nothing automatically.
    /// Any files/dirs given are still collected and become the selector's list.
    #[arg(short, long)]
    select: bool,

    /// How to map emulator screen onto window: `stretch`, `fit`, `zoom`, or a
    /// scale factor like `2` or `2.5` (fractional allowed).
    #[arg(long, value_parser = parse_scale_mode, default_value = "fit")]
    scale: ScaleModeArg,

    /// How to fill the border outside the image.
    #[arg(long, value_enum, default_value_t = BorderModeArg::Black)]
    border: BorderModeArg,

    /// Shader used to render the emulator screen. Defaults to the
    /// LCD shader for Game Boy / GBA titles and the Lottes CRT shader otherwise.
    #[arg(long, value_enum)]
    shader: Option<ShaderArg>,

    /// Path to a libretro `.slangp` shader preset to use instead of `--shader`,
    /// e.g. any preset from the slang-shaders repo. Takes precedence over
    /// `--shader`.
    #[arg(long)]
    slangp: Option<PathBuf>,

    /// Only load db entries with a field matching this regex, e.g.
    /// `-I '(Demo|Intro)'`. Matched against each field of the db line on its
    /// own, so it can pick on any one of them but never spans two.
    /// Repeatable; all patterns must match.
    #[arg(short = 'I', long, value_parser = Regex::new)]
    include: Vec<Regex>,

    /// Exclude db entries with a field matching this regex, e.g.
    /// `-X 'category:.*Disk'`. Matched against each field of the db line on
    /// its own, so it can pick on any one of them but never spans two.
    /// Repeatable; a match on any pattern excludes.
    #[arg(short = 'X', long, value_parser = Regex::new)]
    exclude: Vec<Regex>,

    /// Shuffle the list of files into a random order.
    #[arg(long)]
    shuffle: bool,

    /// When to show overlay info text
    #[arg(long, value_enum, default_value_t = InfoDisplay::OnMulti)]
    info: InfoDisplay,

    /// Amiga: Force AGA (A1200 with 8MB Fast RAM)
    #[arg(long)]
    aga: bool,

    /// Atari ST: Force STE
    #[arg(long)]
    ste: bool,

    /// Amiga: Force high specs (68030 + FPU)
    #[arg(long)]
    fast: bool,

    /// Amiga/Atari ST: add extra memory
    #[arg(long)]
    xmem: bool,

    /// C64: Always use JiffyDOS to load
    /// Amiga: Turn off disk rotation emulation
    #[arg(long, verbatim_doc_comment)]
    fast_load: bool,

    /// Amiga,C64,Amstrad: Dont produce disk loading sound
    #[arg(long)]
    silent_drive: bool,

    /// Open windowed instead of full screen
    #[arg(short, long)]
    window: bool,

    /// Max number of seconds to play a file before skipping
    #[arg(long)]
    max_time: Option<usize>,

    /// Force vsync, slowing down or speeding up emulation to fit
    #[arg(long)]
    force_vsync: bool,

    /// Benchmark: run emulation unthrottled (no vsync, audio dropped) for two
    /// seconds, print the number of frames stepped, then exit.
    #[arg(long)]
    speed_test: bool,

    /// Max queued frames. Lower values = better input response
    #[arg(long, default_value_t = 2)]
    latency: u32,

    /// Extra options to add to libretro
    #[arg(short = 'x', long, value_delimiter = ',')]
    extra_options: Vec<String>,

    /// Render multiple emulators in a COLSxROWS grid, e.g. --grid=5x4
    #[arg(long, value_parser = parse_grid)]
    grid: Option<(u32, u32)>,

    /// Background clear color as a hex string, e.g. `#003` or `000080`.
    #[arg(long, value_parser = parse_color, default_value = "000033")]
    clear_color: Color,

    /// C64: Add ram expansion unit (16MB)
    #[arg(long)]
    reu: bool,

    /// ILBM: Animate colour-cycling (CRNG) ranges. Off by default.
    #[arg(short = 'C', long)]
    color_cycle: bool,

    /// Commodore variant (Only C64 well supported)
    #[arg(long, value_enum, default_value_t = CbmSystem::C64)]
    cbm_variant: CbmSystem,

    /// Don't silence libretro cores' stdout/stderr (for debugging)
    #[arg(long)]
    no_silence: bool,

    /// Skip bad files, loop playlist and detect idle demos
    #[arg(long)]
    tv_mode: bool,

    /// Skip demo after still screen and no audio
    #[arg(long, default_value_t = 0)]
    idle_timeout: i32,
}

/// Parse a hex color string like `#003`, `#000080`, or `000080` into a [`Color`].
fn parse_color(s: &str) -> Result<Color, String> {
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
    Ok(Color::srgb_u8(parse(r)?, parse(g)?, parse(b)?))
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
enum InfoDisplay {
    /// Always show demo info on start
    Always,
    /// Dont show demo info on start
    Never,
    /// Show demo info on start with multiple files
    OnMulti,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum ScaleModeArg {
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
enum CbmSystem {
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
enum ShaderArg {
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
    fn path(self) -> &'static str {
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
enum BorderModeArg {
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
struct RenderSettings {
    border_mode: BorderMode,
    scale_mode: ScaleMode,
    crt_effect: bool,
}

#[derive(Resource, Default)]
struct AppSettings {
    show_info: bool,
    files: Vec<EmuFile>,
    current_game: isize,
    current_emu: usize,
    maximized: bool,
    all_emus: bool,
    last_draw: f64,
    text_list: Option<Entity>,
    file_list: Option<Entity>,
    /// The file picker's search index, built lazily from `files` on first open
    /// and reused (cheap `Arc` clone) on every open after that — building the
    /// trigram index over the whole list is the picker's expensive step.
    file_source: Option<IndexedSource>,
    hotkey_pressed: f32,
    mouse_index: Option<usize>,
    speed_test: bool,
    tv_mode: bool,
    idle_timeout: i32,
}

fn enter_fullscreen(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Current);
}

/// A `Write` that targets a raw fd directly, bypassing Rust's `Stdout`. Used to
/// keep logging going after we've pointed fd 1 at `/dev/null`. `Copy` so it can
/// be handed out repeatedly by a `MakeWriter` closure without owning the fd.
#[cfg(unix)]
#[derive(Clone, Copy)]
pub(crate) struct FdWriter(std::os::fd::RawFd);

#[cfg(unix)]
impl std::io::Write for FdWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // SAFETY: `self.0` is a live fd (the dup of the original stdout, kept
        // open for the process lifetime); the buffer is valid for `buf.len()`.
        let n = unsafe { libc::write(self.0, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
static SAVED_STDOUT: std::sync::OnceLock<FdWriter> = std::sync::OnceLock::new();

/// Print a line to the real terminal, bypassing the `/dev/null` redirect that
/// [`silence_stdout`] installs on fd 1 to muzzle the libretro cores. Falls back
/// to the process stdout when nothing was redirected.
pub(crate) fn println(text: impl std::fmt::Display) {
    // Format up front and write once, so a line can't be torn in half by
    // another thread's write between the text and its newline.
    let mut line = text.to_string();
    line.push('\n');
    #[cfg(unix)]
    if let Some(mut writer) = SAVED_STDOUT.get().copied() {
        use std::io::Write;
        let _ = writer.write_all(line.as_bytes());
        return;
    }
    use std::io::Write;
    let _ = std::io::stdout().write_all(line.as_bytes());
}

/// Raise the process's soft open-file limit to the hard limit (or a large
/// fallback), best-effort. Some bundled libretro cores (notably the Amiga
/// `puae` core) leak POSIX named semaphores across reloads — every file
/// switch loads a fresh core instance, and each one opens sync semaphores
/// under the same PID-keyed names the previous instance never closed. macOS
/// defaults to a stingy 256-fd soft limit, which that leak exhausts after only
/// a few dozen Amiga files; Linux's much larger default rarely notices. This
/// doesn't stop the leak, it just buys enough headroom that a normal session
/// won't hit it.
#[cfg(unix)]
fn raise_fd_limit() {
    const FALLBACK_LIMIT: libc::rlim_t = 65536;
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return;
        }
        let target = if limit.rlim_max == libc::RLIM_INFINITY {
            FALLBACK_LIMIT
        } else {
            limit.rlim_max
        };
        if target > limit.rlim_cur {
            limit.rlim_cur = target;
            libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
        }
    }
}

/// Silence stdout *and* stderr for the rest of the process by redirecting fds 1
/// and 2 to `/dev/null`, so libretro cores' `printf`/`fprintf`/`puts` output is
/// discarded. Returns a `FdWriter` over a dup of the *original* stdout so tracing
/// can keep writing to the real terminal. Redirecting (rather than `close`ing the
/// fds) is deliberate: it keeps them valid, so a later `open` can't reuse them and
/// get scribbled on by a core.
#[cfg(unix)]
fn silence_stdout() -> std::io::Result<FdWriter> {
    use std::os::fd::{AsFd, AsRawFd, IntoRawFd};

    // Duplicate the current stdout; the dup outlives this call (never closed).
    let saved = std::io::stdout().as_fd().try_clone_to_owned()?;
    let devnull = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
    // SAFETY: dup2 onto STDOUT_FILENO/STDERR_FILENO; all fds are valid for the call.
    for target in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        if unsafe { libc::dup2(devnull.as_raw_fd(), target) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(FdWriter(saved.into_raw_fd()))
}

/// Keep the OpenMP-using cores (bsnes, bsnes-hd, flycast) from eating the
/// machine.
///
/// Those cores parallelise tiny per-frame regions — bsnes renders scanlines
/// with an `omp parallel for` — but libgomp defaults to *active* waiting, so
/// its `nproc - 1` workers busy-spin between regions. On a 48-thread box that
/// is ~25 cores burned to render 224 scanlines, and it is also slower than not
/// spinning at all (bsnes measured 294 fps spinning vs 377 fps passive).
/// Passive waiting parks the workers instead, and a small pool avoids waking
/// and joining dozens of threads per frame for work that never fills them.
///
/// Must run before the core is `dlopen`ed: libgomp reads these once, when its
/// first parallel region starts.
fn tame_openmp_cores() {
    for (key, value) in [("OMP_WAIT_POLICY", "passive"), ("OMP_NUM_THREADS", "4")] {
        // Leave anything the user set on the command line alone.
        if std::env::var_os(key).is_none() {
            // SAFETY: single-threaded here — this is the first thing `main`
            // does, before any thread is spawned.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

fn main() {
    tame_openmp_cores();

    #[cfg(unix)]
    raise_fd_limit();

    // Parse args before touching stdout/stderr so clap's help/errors are visible,
    // and so `--no-silence` can be honoured when setting up logging below.
    let mut args = Args::parse();
    if cfg!(debug_assertions) {
        args.no_silence = true;
    }

    // On Unix, silence the cores by redirecting stdout/stderr to /dev/null and
    // route tracing to a dup of the original stdout, unless `--no-silence` asks
    // us to leave them alone (for debugging core output).
    #[cfg(unix)]
    let saved_stdout = if args.no_silence {
        None
    } else {
        silence_stdout().ok()
    };
    // Hand the dup to `println` so the rest of the app can still print.
    #[cfg(unix)]
    if let Some(writer) = saved_stdout {
        let _ = SAVED_STDOUT.set(writer);
    }

    // Under `--features profile` the subscriber is built by Bevy's `LogPlugin`
    // instead (see `profiling::log_plugin`), because that's where the
    // chrome-trace layer that records the ECS spans is installed. Setting one
    // here too would just lose the race and log an error.
    #[cfg(not(feature = "profile"))]
    {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new(if cfg!(debug_assertions) {
                "demarc=debug,warn"
            } else {
                "error"
            })
        });
        let builder = tracing_subscriber::fmt()
            .with_ansi(cfg!(not(target_os = "windows")))
            .with_env_filter(filter)
            .with_target(true)
            .compact();
        #[cfg(unix)]
        match saved_stdout {
            Some(writer) => builder.with_writer(move || writer).init(),
            // Silencing disabled or redirect failed: use the default stdout writer.
            None => builder.init(),
        }
        #[cfg(not(unix))]
        builder.init();
    }

    // Trim the download cache before anything fetches into it, so this run's
    // own downloads can't be evicted out from under it.
    fetch::prune_cache();

    // Expand any directory in `games` into the `.m3u` files found within it.
    let mut files = Vec::with_capacity(args.files.len());

    // Load entries from a tab-separated demo database (id, title, author, date,
    // party, category, tags, download — named or in that order). Each URL is
    // fetched on demand when loaded.
    let filter = DbFilter {
        include: &args.include,
        exclude: &args.exclude,
    };
    if let Some(db) = &args.db {
        let path = Path::new(db);
        if !path.exists() {
            println(format!("** Error: Can't load database {path:?}"));
            return;
        }
        collect_db(path, &filter, &mut files).unwrap();
    }
    // Anything piped in is a db too, so it can be filtered before loading.
    collect_db_stdin(&filter, &mut files).unwrap();

    for file in std::mem::take(&mut args.files) {
        // Download HTTP(S) URLs to the local cache and continue with the file,
        // so demarc can be launched directly with a link from a browser.
        let file = match file.to_str() {
            Some(s) if fetch::is_url(s) => match fetch::fetch_url(s) {
                Ok(path) => path,
                Err(e) => {
                    tracing::error!("Failed to download {s}: {e}");
                    continue;
                }
            },
            _ => file,
        };
        if file.is_dir() {
            let len = files.len();
            collect_files(&file, &mut files, args.many).unwrap();
            if len == files.len() {
                files.push(collect_file(&file).unwrap());
            }
        } else {
            files.push(collect_file(&file).unwrap());
        }
    }

    if args.shuffle {
        use rand::seq::SliceRandom;
        files.shuffle(&mut rand::rng());
    }

    let multiple = files.len() > 1;
    let mut window = Window {
        title: "Demarc".into(),
        present_mode: if args.speed_test {
            PresentMode::AutoNoVsync
        } else {
            PresentMode::Fifo
        },
        mode: if args.window {
            WindowMode::Windowed
        } else {
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        },
        resizable: false,
        ..Default::default()
    };
    if args.window {
        window.resolution = (720, 540).into();
    }
    let primary_window = Some(window);

    let shader = args.shader.unwrap_or(ShaderArg::Lottes);

    // A user-supplied `--slangp` wins; otherwise resolve the bundled shader by
    // name — a `.wgsl` path selects the single-pass WGSL backend, anything
    // else a `.slangp` preset run through librashader. The slangp passthrough
    // (`stock.slangp`) is always the bundled one.
    let passthrough = system_dir().join("shaders/slangp/stock.slangp");
    let shader_path = match &args.slangp {
        Some(path) => ShaderPath::Slangp {
            effect: path.clone(),
            passthrough,
        },
        None if shader.path().ends_with(".wgsl") => ShaderPath::Wgsl {
            asset_path: shader.path().into(),
        },
        None => ShaderPath::Slangp {
            effect: system_dir().join(shader.path()),
            passthrough,
        },
    };

    let render_settings = RenderSettings {
        border_mode: args.border.into(),
        scale_mode: args.scale.into(),
        // `--shader none` starts with the shaders disabled; an
        // explicit `--slangp` always enables it.
        crt_effect: args.slangp.is_some() || !matches!(shader, ShaderArg::None),
    };
    let settings = AppSettings {
        current_game: -1,
        show_info: args.info == InfoDisplay::Always
            || (multiple && args.info == InfoDisplay::OnMulti),
        files,
        maximized: args.grid.is_none(),
        speed_test: args.speed_test,
        tv_mode: args.tv_mode,
        idle_timeout: args.idle_timeout,
        ..Default::default()
    };

    let win = args.window;
    let clear_color = args.clear_color;

    let speed_test = args.speed_test;
    let mut app = App::new();
    if speed_test {
        // Drive the update loop as fast as possible regardless of window focus.
        app.insert_resource(bevy::winit::WinitSettings::continuous());
    }
    // `main` installs its own tracing subscriber above, so the default one is
    // dropped — except in a profiling build, where `LogPlugin` owns the
    // subscriber (it carries the chrome-trace layer) and gets our writer.
    let default_plugins = DefaultPlugins.build();
    #[cfg(not(feature = "profile"))]
    let default_plugins = default_plugins.disable::<bevy::log::LogPlugin>();
    #[cfg(feature = "profile")]
    let default_plugins = default_plugins.set(profiling::log_plugin(
        #[cfg(unix)]
        saved_stdout,
        #[cfg(not(unix))]
        None,
    ));

    app.insert_resource(args)
        .insert_resource(settings)
        .insert_resource(render_settings)
        .insert_resource(ClearColor(clear_color))
        .add_plugins((
            default_plugins
                // Bevy's default compute pool grabs every remaining core and runs
                // the multi-threaded ECS executor across all of them. This app has
                // only a handful of trivial systems and is GPU-bound plus one
                // dedicated emulator worker thread, so those extra threads spend
                // their time coordinating (task-queue push/pop, mutex contention)
                // rather than computing — ~34% of total CPU on a 24-core machine.
                // Capping the pool at 2 removes that spin with no throughput cost.
                .set(bevy::app::TaskPoolPlugin {
                    task_pool_options: bevy::app::TaskPoolOptions {
                        compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                            min_threads: 1,
                            max_threads: 2,
                            percent: 0.0,
                            on_thread_spawn: None,
                            on_thread_destroy: None,
                        },
                        ..Default::default()
                    },
                })
                .set(WindowPlugin {
                    primary_window,
                    ..Default::default()
                })
                // Load assets from the extracted `system` dir so they can ship
                // inside `system.zip` (embedded in the binary) rather than a
                // loose `assets/` folder next to the executable.
                .set(AssetPlugin {
                    file_path: system_dir().to_string_lossy().into_owned(),
                    ..Default::default()
                }),
            RetroPlugin {},
            CommandPlugin,
            PostProcessPlugin {
                shader: shader_path,
            },
            HudPlugin,
            TextInputPlugin,
            FuzzyListPlugin,
            ScreenSaverPlugin,
            SpeedTestPlugin,
        ));
    #[cfg(feature = "profile")]
    app.add_plugins(profiling::ProfilingPlugin);
    // `RetroPlugin::fix_window` unconditionally forces `Windowed` at Startup
    // (so early setup systems see a stable, non-transitional window size);
    // this restores the actually-requested fullscreen mode afterward.
    if !win {
        app.add_systems(PostStartup, enter_fullscreen);
    }
    app.run();
}
