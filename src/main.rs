#![allow(dead_code, clippy::too_many_arguments, clippy::type_complexity)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use bevy::render::extract_resource::ExtractResource;
use bevy::window::WindowMode;
use bevy::{prelude::*, window::PresentMode};
use bevy_tweening::TweeningPlugin;
use clap::builder::styling::{AnsiColor, Style};
use clap::builder::{Styles, styling};
use clap::{ColorChoice, Parser};

#[allow(warnings)]
mod libretro;

mod audio;
mod hud;
mod post_process;
mod retro;
mod retro_emu;
mod screensaver;
mod utils;

use hud::HudPlugin;
use post_process::{BorderMode, PostProcessPlugin, ScaleMode};
use retro::{RetroPlugin, system_dir};
use screensaver::ScreenSaverPlugin;
use tracing::Level;
use tracing_subscriber::EnvFilter;

const STYLES: Styles = Styles::styled()
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
#[command(name = "demarc", styles = STYLES, color = ColorChoice::Always, about = "Bevy + libretro front-end")]
struct Args {
    /// Path to the programs/disks to load
    files: Vec<PathBuf>,

    /// How to map the low-res render target onto the window.
    #[arg(long, value_enum, default_value_t = ScaleModeArg::Fit)]
    scale: ScaleModeArg,

    /// How to fill the border outside the image (letterbox/pillarbox bars).
    #[arg(long, value_enum, default_value_t = BorderModeArg::Black)]
    border: BorderModeArg,

    /// Shuffle the list of programs into a random order.
    #[arg(long)]
    shuffle: bool,

    /// When to show overlay info text
    #[arg(long, value_enum, default_value_t = InfoDisplay::OnMulti)]
    info: InfoDisplay,

    /// Amiga: Force AGA (A1200 with 8MB Fast RAM)
    #[arg(long)]
    aga: bool,

    /// Atari: Force STE
    #[arg(long)]
    ste: bool,

    /// Amiga: Force high specs (68030 + FPU + 128MB Z3 RAM)
    #[arg(long)]
    high: bool,

    /// Open windowed
    #[arg(long)]
    window: bool,

    /// Max number of seconds to play a file before skipping
    #[arg(long)]
    max_time: Option<usize>,

    /// Force vsync, slowing down or speeding up emulation to fit
    #[arg(long)]
    force_vsync: bool,

    /// Extra options to add to libretro
    #[arg(long, value_delimiter = ',')]
    extra_options: Vec<String>,

    /// Grid rendering
    #[arg(long)]
    grid: bool,
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
enum BorderModeArg {
    /// Stretch the edge pixels outward into the border.
    Stretch,
    /// Fill the border with black.
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
}

/// Recursively collect all `.m3u` files under `dir` into `out`.
fn collect_m3u_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            warn!("Failed to read directory {}: {err}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_m3u_files(&path, out);
        } else if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u"))
        {
            out.push(path);
        }
    }
}

fn main() {
    let mut args = Args::parse();

    // Expand any directory in `games` into the `.m3u` files found within it.
    let mut games = Vec::with_capacity(args.files.len());
    for game in std::mem::take(&mut args.files) {
        if game.is_dir() {
            let len = games.len();
            collect_m3u_files(&game, &mut games);
            if len == games.len() {
                games.push(game);
            }
        } else {
            games.push(game);
        }
    }
    args.files = games;

    if args.shuffle {
        use rand::seq::SliceRandom;
        args.files.shuffle(&mut rand::rng());
    }

    let multiple = args.files.len() > 1;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(if cfg!(debug_assertions) {
            "demarc=info,warn"
        } else {
            "error"
        })
    });

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
    let primary_window = Some(Window {
        title: "Demarc".into(),
        present_mode: PresentMode::Fifo,
        mode: if args.window {
            WindowMode::Windowed
        } else {
            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
        },
        resolution: (720, 540).into(),
        resizable: false,
        ..Default::default()
    });

    let settings = AppSettings {
        border_mode: args.border.into(),
        scale_mode: args.scale.into(),
        crt_effect: true,
        show_info: args.info == InfoDisplay::Always
            || (multiple && args.info == InfoDisplay::OnMulti),
    };

    App::new()
        .insert_resource(args)
        .insert_resource(settings)
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
            PostProcessPlugin,
            TweeningPlugin,
            HudPlugin,
            ScreenSaverPlugin,
        ))
        .run();
}
