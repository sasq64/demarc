// Needed for bevy systems
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use std::path::Path;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{prelude::*, window::PresentMode};
use clap::Parser;
use regex::Regex;

#[allow(warnings)]
mod libretro;

mod audio;
mod backend;
mod cache;
mod cbmconvert;
mod commands;
mod config;
mod degas;
mod egui_ui;
mod emu_file;
mod emulator;
mod fetch;
mod files;
mod frontend;
mod fuzzy_list;
mod ilbm;
mod image_emu;
mod jobs;
mod libloader;
mod load_error;
mod m3u;
mod media_keys;
mod music_emu;
mod music_vis;
mod newsys;
mod overrides;
mod pixels;
mod post_process;
mod retro_emu;
mod screensaver;
mod speed_test;
mod system_dir;
mod utils;
mod workfile;
mod zx_scr;

#[cfg(feature = "flash")]
mod flash_emu;
#[cfg(feature = "profile")]
mod profiling;
#[cfg(target_os = "linux")]
mod wine_emu;

use commands::CommandPlugin;
use files::{DbFilter, collect_db, collect_db_stdin, collect_file, collect_files};
use frontend::RetroPlugin;
use newsys::NewSys;
use post_process::{DOWNSAMPLE_PRESET, PostProcessPlugin, ShaderPath};
use screensaver::ScreenSaverPlugin;
use speed_test::SpeedTestPlugin;
use system_dir::system_dir;

#[cfg(not(feature = "profile"))]
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

/// Keep glibc's per-thread malloc arenas from crowding the address space a
/// JIT core needs for its translation cache.
///
/// The x86-64 JIT in the Amiga cores (Amiberry, p-uae) addresses the emulator's
/// globals RIP-relative, so its translation cache has to land within ±2GB of
/// them. Amiberry reserves 4GB of "natmem" immediately below the core's
/// library, which eats the whole window on that side, leaving only the space
/// above. glibc reserves 64MB of address space per malloc arena and allows
/// `8 * nproc` of them, so on a many-core box demarc's own threads wall that
/// window off. The core's 16MB request then fails and its allocator halves it
/// until something fits — measured here as an **8KB** cache, which thrashes:
/// TBL's Starstruck rendered visibly slower for it.
///
/// A cap costs a little allocator concurrency and buys the window back (16MB
/// cache, zero failed allocations). Must run before any thread allocates,
/// which is why it is the first thing `main` does.
#[cfg(all(unix, target_env = "gnu"))]
fn cap_malloc_arenas() {
    // Leave an explicit choice on the command line alone, the way
    // `tame_openmp_cores` does.
    if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
        return;
    }
    // SAFETY: `mallopt` is thread-safe, and nothing has spawned a thread yet.
    unsafe { libc::mallopt(libc::M_ARENA_MAX, 8) };
}

fn main() {
    #[cfg(all(unix, target_env = "gnu"))]
    cap_malloc_arenas();
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

    // Trim the caches before anything writes into them, so this run's own
    // downloads and built discs can't be evicted out from under it.
    fetch::prune_cache();
    libloader::prune_cache();
    newsys::prune_caches();

    // Expand any directory in `games` into the `.m3u` files found within it.
    let mut files = Vec::with_capacity(args.files.len());

    // Load entries from a tab-separated demo database (id, title, author, date,
    // party, category, tags, download — named or in that order). Each URL is
    // fetched on demand when loaded.

    // A Windows demo takes the whole screen for its own wine + gamescope
    // session, so it has nothing to render into a grid cell: leave those
    // entries out of a grid rather than let one blank the grid while it runs.
    let mut exclude = args.exclude.clone();
    if args.grid.is_some() {
        exclude.push(Regex::new("(?i)^platform:windows$").unwrap());
    }
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
        if file.is_dir() && args.collect {
            let len = files.len();
            collect_files(&file, &mut files, args.many).unwrap();
            if len == files.len() {
                files.push(collect_file(&file).unwrap());
            }
        } else {
            files.push(collect_file(&file).unwrap());
        }
    }

    // `--shuffle` is the older spelling of `--sort random`; an explicit
    // `--sort` wins over it.
    match args.sort.or(args.shuffle.then_some(SortArg::Random)) {
        Some(SortArg::Random) => {
            use rand::seq::SliceRandom;
            files.shuffle(&mut rand::rng());
        }
        // Ranks are positions, so the best comes first. Entries without a rank
        // sort last, keeping the order they were collected in.
        Some(SortArg::Rank) => files.sort_by_key(|f| f.game_info.rank.unwrap_or(u32::MAX)),
        None => {}
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
    // else a `.slangp` preset run through librashader.
    let downsample = system_dir().join(DOWNSAMPLE_PRESET);
    let shader_path = match &args.slangp {
        Some(path) => ShaderPath::Slangp {
            effect: path.clone(),
            downsample,
            downsample_limit: args.downsample,
        },
        None if shader.path().ends_with(".wgsl") => ShaderPath::Wgsl {
            asset_path: shader.path().into(),
        },
        None => ShaderPath::Slangp {
            effect: system_dir().join(shader.path()),
            downsample,
            downsample_limit: args.downsample,
        },
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
                // Capping the pool at 2 removes that spin with no throughput cost.
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
            egui_ui::EguiUiPlugin,
            ScreenSaverPlugin,
            SpeedTestPlugin,
            jobs::JobsPlugin,
        ));
    #[cfg(feature = "profile")]
    app.add_plugins(profiling::ProfilingPlugin);
    // A Windows demo takes the screen off demarc while it runs; this puts it
    // back afterwards.
    #[cfg(target_os = "linux")]
    app.add_plugins(wine_emu::WinePlugin);
    // `RetroPlugin::fix_window` unconditionally forces `Windowed` at Startup
    // (so early setup systems see a stable, non-transitional window size);
    // this restores the actually-requested fullscreen mode afterward.
    if !win {
        app.add_systems(PostStartup, enter_fullscreen);
    }
    app.run();
}
