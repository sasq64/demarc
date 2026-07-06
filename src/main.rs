#![allow(dead_code, clippy::too_many_arguments, clippy::type_complexity)]
use std::path::PathBuf;

use bevy::render::extract_resource::ExtractResource;
use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{prelude::*, window::PresentMode};
use clap::builder::styling::{AnsiColor, Style};
use clap::builder::{Styles, styling};
use clap::{ColorChoice, Parser};

#[allow(warnings)]
mod libretro;

mod audio;
mod commands;
mod emulator;
#[cfg(feature = "flash")]
mod flash_emu;
mod hud;
mod ilbm;
mod image_emu;
mod libloader;
mod media_keys;
mod post_process;
mod retro;
mod retro_emu;
mod screensaver;
mod text_input;
mod utils;

use commands::CommandPlugin;
use hud::HudPlugin;
use post_process::{BorderMode, PostProcessPlugin, ScaleMode};
use retro::{RetroPlugin, system_dir};
use screensaver::ScreenSaverPlugin;
use text_input::TextInputPlugin;
use tracing_subscriber::EnvFilter;

use crate::utils::collect_files;

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
    /// Path to the files to load
    files: Vec<PathBuf>,

    /// Treat disk images in same dir as separate files
    #[arg(long)]
    many: bool,

    /// How to map emulator screen onto window.
    #[arg(long, value_enum, default_value_t = ScaleModeArg::Fit)]
    scale: ScaleModeArg,

    /// How to fill the border outside the image.
    #[arg(long, value_enum, default_value_t = BorderModeArg::Black)]
    border: BorderModeArg,

    /// Post-process shader used to render the emulator screen. Defaults to the
    /// LCD shader for Game Boy / GBA titles and the Lottes CRT shader otherwise.
    #[arg(long, value_enum)]
    shader: Option<ShaderArg>,

    /// Path to a libretro `.slangp` shader preset to use instead of `--shader`,
    /// e.g. any preset from the slang-shaders repo. Takes precedence over
    /// `--shader`.
    #[arg(long)]
    preset: Option<PathBuf>,

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

    /// Max queued frames. Lower values = better input response
    #[arg(long, default_value_t = 2)]
    latency: u32,

    /// Don't delay video to match audio output latency. Lowers video latency,
    /// but audio may lag video on high-latency sinks (e.g. Bluetooth)
    #[arg(long)]
    no_av_sync: bool,

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
    #[arg(long)]
    color_cycle: bool,

    /// Commodore variant (Only C64 well supported)
    #[arg(long, value_enum, default_value_t = CbmSystem::C64)]
    cbm_variant: CbmSystem,
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

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum ScaleModeArg {
    /// Fill the window, distorting the aspect ratio.
    Stretch,
    /// Preserve aspect ratio, adding letterbox/pillarbox bars.
    Fit,
    /// Preserve aspect ratio, cropping top/bottom or left/right to fill.
    Zoom,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum CbmSystem {
    /// Default Commodore C64
    C64,
    /// Commodore 128
    C128,
    /// C64 DTV Stick
    Dtv,
}

impl From<ScaleModeArg> for ScaleMode {
    fn from(s: ScaleModeArg) -> Self {
        match s {
            ScaleModeArg::Stretch => ScaleMode::Stretch,
            ScaleModeArg::Fit => ScaleMode::Fit,
            ScaleModeArg::Zoom => ScaleMode::Zoom,
        }
    }
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
enum ShaderArg {
    /// Timothy Lottes CRT shader — scanlines/shadow mask, for CRT-era systems.
    Lottes,
    /// cgwg dot-matrix LCD grid shader, for handheld LCD systems.
    Lcd,
    /// Lightweight single-pass LCD grid shader (zfast-lcd).
    LcdSimple,
}

impl ShaderArg {
    /// Path of the `.slangp` preset, relative to the `system` asset directory.
    /// These are RetroArch libretro presets bundled under `shaders/slangp/`
    /// (see `system/shaders/slangp/`) and run through librashader.
    fn path(self) -> &'static str {
        match self {
            ShaderArg::Lottes => "shaders/slangp/crt/crt-lottes.slangp",
            ShaderArg::Lcd => "shaders/slangp/handheld/lcd-grid-v2.slangp",
            ShaderArg::LcdSimple => "shaders/slangp/handheld/zfast-lcd.slangp",
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

#[derive(Resource, Default, Clone, ExtractResource)]
struct AppSettings {
    border_mode: BorderMode,
    scale_mode: ScaleMode,
    crt_effect: bool,
    show_info: bool,
    games: Vec<PathBuf>,
    current_game: isize,
    max_time: Option<usize>,
    current_emu: usize,
    maximized: bool,
    all_emus: bool,
    /// Delay video to match audio-output latency (see `--no-av-sync`).
    av_sync: bool,
    last_draw: f64,
    text_list: Option<Entity>,
    hotkey_pressed: f32,
    mouse_index: Option<usize>,
}

fn enter_fullscreen(mut window: Single<&mut Window, With<PrimaryWindow>>) {
    window.mode = WindowMode::BorderlessFullscreen(MonitorSelection::Current);
}

fn auto_screenshot(
    mut commands: Commands,
    time: Res<Time>,
    mut shot: bevy::prelude::Local<bool>,
    mut exit: bevy::prelude::MessageWriter<bevy::app::AppExit>,
) {
    use bevy::render::view::screenshot::{Screenshot, save_to_disk};
    let secs = std::env::var("AUTO_SHOT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4.0);
    let t = time.elapsed_secs();
    if !*shot && t >= secs {
        *shot = true;
        let path = std::env::var("AUTO_SHOT").unwrap();
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(path));
    }
    if t >= secs + 1.5 {
        exit.write(bevy::app::AppExit::Success);
    }
}

fn main() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            "demarc=debug,warn"
        } else {
            "error"
        })
    });
    tracing_subscriber::fmt()
        .with_ansi(cfg!(not(target_os = "windows")))
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
    let mut args = Args::parse();

    // Expand any directory in `games` into the `.m3u` files found within it.
    let mut games = Vec::with_capacity(args.files.len());
    for game in std::mem::take(&mut args.files) {
        if game.is_dir() {
            let len = games.len();
            collect_files(&game, &mut games, args.many);
            if len == games.len() {
                games.push(game);
            }
        } else {
            games.push(game);
        }
    }
    if args.shuffle {
        use rand::seq::SliceRandom;
        games.shuffle(&mut rand::rng());
    }

    let multiple = games.len() > 1;
    let mut window = Window {
        title: "Demarc".into(),
        present_mode: PresentMode::Fifo,
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
    if let Ok(res) = std::env::var("WIN_RES") {
        if let Some((w, h)) = res.split_once('x') {
            window.resolution = (w.parse().unwrap(), h.parse().unwrap()).into();
            window.mode = WindowMode::Windowed;
        }
    }
    let primary_window = Some(window);

    let settings = AppSettings {
        border_mode: args.border.into(),
        scale_mode: args.scale.into(),
        current_game: -1,
        crt_effect: true,
        show_info: args.info == InfoDisplay::Always
            || (multiple && args.info == InfoDisplay::OnMulti),
        games: games.clone(),
        max_time: args.max_time,
        maximized: args.grid.is_none(),
        av_sync: !args.no_av_sync,
        ..Default::default()
    };

    let win = args.window;
    let clear_color = args.clear_color;
    // A user-supplied `--preset` wins; otherwise pick a bundled preset by name,
    // defaulting to the LCD preset for handheld LCD systems (Game Boy / GBA) and
    // the Lottes CRT preset for everything else. Both resolve to an absolute
    // `.slangp` path; the passthrough (`stock.slangp`) is always the bundled one.
    let effect_path = match &args.preset {
        Some(path) => path.clone(),
        None => {
            let shader = args.shader.unwrap_or_else(|| {
                match games.first().map(|g| utils::get_system_type(g)) {
                    Some(utils::SystemType::Gameboy | utils::SystemType::Gba) => ShaderArg::Lcd,
                    _ => ShaderArg::Lottes,
                }
            });
            system_dir().join(shader.path())
        }
    };
    let passthrough_path = system_dir().join("shaders/slangp/stock.slangp");

    let mut app = App::new();
    app.insert_resource(args)
        .insert_resource(settings)
        .insert_resource(ClearColor(clear_color))
        .add_plugins((
            DefaultPlugins
                .build()
                .disable::<bevy::log::LogPlugin>()
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
                effect_path,
                passthrough_path,
            },
            HudPlugin,
            TextInputPlugin,
            ScreenSaverPlugin,
        ));
    if !win && (cfg!(target_os = "windows") || cfg!(target_os = "linux")) {
        app.add_systems(PostStartup, enter_fullscreen);
    }
    if std::env::var("AUTO_SHOT").is_ok() {
        app.add_systems(bevy::app::Update, auto_screenshot);
    }
    app.run();
}
