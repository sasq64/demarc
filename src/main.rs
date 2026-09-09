// Needed for bevy systems
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use std::cmp::Reverse;
use std::path::Path;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{prelude::*, window::PresentMode};
use clap::Parser;

#[allow(warnings)]
mod libretro;

mod audio;
mod backend;
mod cache;
mod cbmconvert;
mod commands;
mod config;
mod degas;
mod demarc_settings;
mod egui_settings;
mod egui_ui;
mod emu_file;
mod emulator;
mod fetch;
mod files;
mod frontend;
mod fuzzy_list;
mod headless;
mod ilbm;
mod image_emu;
mod jobs;
mod libloader;
mod load_error;
mod m3u;
mod media_keys;
mod mouse_cursor;
mod music_emu;
mod music_vis;
mod newsys;
mod overrides;
mod pixels;
mod post_process;
mod remote_control;
mod retro_emu;
mod screensaver;
mod shader_dialog;
mod speed_test;
mod system_dir;
mod tiff_pal;
mod utils;
mod workfile;
mod zx_scr;

#[cfg(feature = "flash")]
mod flash_emu;
#[cfg(target_os = "linux")]
mod wine;
#[cfg(target_os = "linux")]
mod wine_sandbox;

use commands::CommandPlugin;
use egui_settings::AppSettingsExt;
use files::{DbFilter, collect_db, collect_db_stdin, collect_file, collect_files};
use frontend::FrontendPlugin;
use mouse_cursor::MouseCursorPlugin;
use newsys::NewSys;
use post_process::{DOWNSAMPLE_PRESET, PostProcessPlugin, ShaderEffect, ShaderPath};
use remote_control::RemoteControlPlugin;
use screensaver::ScreenSaverPlugin;
use speed_test::SpeedTestPlugin;
use system_dir::system_dir;

use tracing_subscriber::EnvFilter;

use crate::config::{AppSettings, Args, InfoDisplay, RenderSettings, ShaderArg, SortArg};

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
/// [`silence_stdout`] installs on fd 1 to muzzle the libretro cores.
pub(crate) fn println(text: impl std::fmt::Display) {
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

/// Print what `--check-wine` found and give back the exit code for it: 0 when
/// a Windows release could be run here, 1 when one of the three pieces
/// [`wine::check_wine`] looks for is missing.
#[cfg(target_os = "linux")]
fn check_wine_and_report() -> i32 {
    let check = wine::check_wine();
    println(check.report());
    if check.ok() { 0 } else { 1 }
}

/// Elsewhere there is nothing to check: the Windows backend is the gamescope
/// core's, and that is Linux only.
#[cfg(not(target_os = "linux"))]
fn check_wine_and_report() -> i32 {
    println("Windows releases are Linux only.");
    1
}

/// Raise the process's soft open-file limit to the hard limit
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
/// discarded.
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

/// Keep the OpenMP-using cores (bsnes, bsnes-hd, flycast) from eating the machine.
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

/// How many glibc malloc arenas `cap_malloc_arenas` leaves us, and so how many
/// threads can allocate hard at once without queueing on an arena lock.
#[cfg(all(unix, target_env = "gnu"))]
const MALLOC_ARENAS: usize = 8;

/// Keep glibc's per-thread malloc arenas from crowding the address space
/// a JIT core (Amiberry) needs for its translation cache.
#[cfg(all(unix, target_env = "gnu"))]
fn cap_malloc_arenas() {
    // Leave an explicit choice on the command line alone, the way
    // `tame_openmp_cores` does.
    if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
        return;
    }
    // SAFETY: `mallopt` is thread-safe, and nothing has spawned a thread yet.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, MALLOC_ARENAS as libc::c_int) };
}

/// Keep rayon's pool from outrunning those arenas.
///
/// The heavy rayon user in the tree is librashader, which compiles the passes
/// of a `.slangp` in parallel — glslang work that is nearly all allocation.
///
/// Setting the variable rather than calling `ThreadPoolBuilder::build_global`
/// keeps rayon out of demarc's dependencies;
#[cfg(all(unix, target_env = "gnu"))]
fn cap_rayon_threads() {
    // Leave an explicit choice on the command line alone, as above.
    if std::env::var_os("RAYON_NUM_THREADS").is_some() {
        return;
    }
    let threads = std::thread::available_parallelism()
        .map_or(MALLOC_ARENAS, |cores| cores.get().min(MALLOC_ARENAS));
    // SAFETY: single-threaded here — `main` has not spawned a thread yet.
    unsafe { std::env::set_var("RAYON_NUM_THREADS", threads.to_string()) };
}

fn main() {
    #[cfg(all(unix, target_env = "gnu"))]
    cap_malloc_arenas();
    #[cfg(all(unix, target_env = "gnu"))]
    cap_rayon_threads();
    tame_openmp_cores();

    #[cfg(unix)]
    raise_fd_limit();

    // Parse args before touching stdout/stderr so clap's help/errors are visible,
    // and so `--no-silence` can be honoured when setting up logging below.
    let mut args = Args::parse();
    if cfg!(debug_assertions) {
        args.no_silence = true;
    }

    // On Unix, silence the cores by redirecting stdout/stderr to /dev/null
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

    // Asked and answered before anything else happens: nothing is loaded, no
    // window opens, and the exit code says whether Windows releases would run,
    // so this is usable from a script.
    if args.check_wine {
        std::process::exit(check_wine_and_report());
    }

    // Trim the caches before anything writes into them, so this run's own
    // downloads and built discs can't be evicted out from under it.
    fetch::prune_cache();
    libloader::prune_cache();
    newsys::prune_caches();

    // Expand any directory in `games` into the `.m3u` files found within it.
    let mut files = Vec::with_capacity(args.files.len());

    let exclude = args.exclude.clone();
    let filter = DbFilter {
        include: &args.include,
        exclude: &exclude,
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
        if file.is_dir() && args.collect {
            collect_files(&file, &mut files, args.many).unwrap();
        } else {
            files.push(collect_file(&file).unwrap());
        }
    }

    match args.sort {
        Some(SortArg::Random) => {
            use rand::seq::SliceRandom;
            files.shuffle(&mut rand::rng());
        }
        // Ranks are positions, so the best comes first. Entries without a rank
        // sort last, keeping the order they were collected in.
        Some(SortArg::Rank) => files.sort_by_key(|f| f.game_info.rank.wrapping_sub(1)),
        Some(SortArg::Date) => files.sort_by_key(|f| Reverse(f.game_info.date)),
        None => {}
    }

    if args.limit > 0 {
        files.truncate(args.limit);
    }

    if args.skip_count > 0 {
        files.drain(0..args.skip_count);
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
    // `--headless` opens no window at all; everything renders into the
    // offscreen image `HeadlessTarget` holds instead.
    let primary_window = (!args.headless).then_some(window);

    let shader = args.shader.unwrap_or(ShaderArg::Lottes);

    // A user-supplied `--slangp` wins; otherwise resolve the bundled shader by
    // name — a `.wgsl` path selects the single-pass WGSL backend, anything
    // else a `.slangp` preset run through librashader.
    let shader_path = ShaderPath {
        effect: match &args.slangp {
            Some(path) => ShaderEffect::Slangp(path.clone()),
            None => shader.effect(),
        },
        downsample: system_dir().join(DOWNSAMPLE_PRESET),
        downsample_limit: args.downsample,
        params: Default::default(),
    };

    let render_settings = RenderSettings {
        border_mode: args.border.into(),
        scale_mode: args.scale.into(),
        // `--shader none` starts with the shaders disabled; an
        // explicit `--slangp` always enables it.
        crt_effect: args.slangp.is_some() || !matches!(shader, ShaderArg::None),
    };
    let sys = NewSys::new(&args);
    let settings = AppSettings {
        demozoo_overrides: overrides::load_default(),
        boot_file: args.boot_file.clone().map(files::leak),
        system: sys,
        current_game: -1,
        show_info: args.info == InfoDisplay::Always
            || (multiple && args.info == InfoDisplay::OnMulti),
        files,
        maximized: args.grid.is_none() || args.focus_first,
        speed_test: args.speed_test,
        tv_mode: args.tv_mode,
        idle_timeout: args.idle_timeout,
        info_delay: args.info_delay,
        info_duration: args.info_duration,
        crt_limit: args.crt_limit,
        ..Default::default()
    };

    let win = args.window;
    let headless = args.headless;
    let clear_color = args.clear_color;

    let demo_settings = demarc_settings::DemarcSettings {
        fullscreen: !win && !headless,
        latency: args.latency,
        volume: 100.0,
        background: clear_color,
        fast_load: false,
        resolution: demarc_settings::Resolution::Res800x600,
    };

    let speed_test = args.speed_test;
    let mut app = App::new();
    if speed_test {
        // Drive the update loop as fast as possible regardless of window focus.
        app.insert_resource(bevy::winit::WinitSettings::continuous());
    }

    // `main` installs its own tracing subscriber above, so the default one is
    // dropped.
    let mut default_plugins = DefaultPlugins.build().disable::<bevy::log::LogPlugin>();
    if headless {
        default_plugins = default_plugins
            // Nothing here uses Bevy's own audio, but its plugin opens the
            // output device on startup.
            .disable::<bevy::audio::AudioPlugin>()
            // No window, so no event loop either -- which also means demarc
            // runs where there is no display at all. `ScheduleRunnerPlugin`
            // below takes over driving the app.
            .disable::<bevy::winit::WinitPlugin>();
    }

    let max_threads = args.max_threads as usize;
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
                // Capping the pool removes that spin with no throughput cost.
                .set(bevy::app::TaskPoolPlugin {
                    task_pool_options: bevy::app::TaskPoolOptions {
                        compute: bevy::app::TaskPoolThreadAssignmentPolicy {
                            min_threads: 1,
                            max_threads,
                            percent: 0.5,
                            on_thread_spawn: None,
                            on_thread_destroy: None,
                        },
                        ..Default::default()
                    },
                })
                .set(WindowPlugin {
                    primary_window,
                    // Without this, having no window is "all windows closed".
                    exit_condition: if headless {
                        bevy::window::ExitCondition::DontExit
                    } else {
                        bevy::window::ExitCondition::OnAllClosed
                    },
                    ..Default::default()
                })
                // Load assets from the extracted `system` dir so they can ship
                // inside `system.zip` (embedded in the binary) rather than a
                // loose `assets/` folder next to the executable.
                .set(AssetPlugin {
                    file_path: system_dir().to_string_lossy().into_owned(),
                    ..Default::default()
                }),
            FrontendPlugin {},
            CommandPlugin,
            PostProcessPlugin {
                shader: shader_path,
            },
            egui_ui::EguiUiPlugin,
            ScreenSaverPlugin,
            MouseCursorPlugin,
            SpeedTestPlugin,
            RemoteControlPlugin,
            jobs::JobsPlugin,
            shader_dialog::ShaderDialogPlugin,
        ));
    if headless {
        // Nothing drives the loop with winit gone. Emulation paces itself, so
        // this only needs to update about as often as a display would.
        let wait = if speed_test {
            std::time::Duration::ZERO
        } else {
            std::time::Duration::from_secs_f64(1.0 / 60.0)
        };
        app.add_plugins(bevy::app::ScheduleRunnerPlugin::run_loop(wait));
        let target = {
            let mut images = app.world_mut().resource_mut::<Assets<Image>>();
            headless::HeadlessTarget::new(&mut images)
        };
        app.insert_resource(target);
    }
    // The settings dialog, registered per settings type. `DemarcSettings` is
    // the one the RightAlt+E hotkey opens.
    app.insert_resource(demo_settings)
        .add_settings_type::<demarc_settings::DemarcSettings>()
        .add_systems(Update, demarc_settings::apply_settings);
    // `RetroPlugin::fix_window` unconditionally forces `Windowed` at Startup
    // (so early setup systems see a stable, non-transitional window size);
    // this restores the actually-requested fullscreen mode afterward.
    if !win && !headless {
        app.add_systems(PostStartup, enter_fullscreen);
    }
    app.run();
}
