//! Mirrors demozoo productions into `<out>/demozoo/<id>/<platform>-<filename>`.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use database::emu_file::{
    Download, EmuFile, FileSource, cache_key, release_downloads, url_file_name,
};
use database::files::{DbFilter, collect_db};
use database::overrides;
use newsys::{NewSys, Override, SysOpts};
use release_io::fetch::{cached_download, download_uncached};

#[derive(Parser)]
struct Args {
    /// Database to walk.
    #[arg(long, default_value = "demodb/demozoo.txt")]
    db: PathBuf,
    /// Output directory.
    #[arg(long, default_value = "/data/mirror")]
    out: PathBuf,
    /// Only these production ids.
    #[arg(long)]
    id: Vec<String>,
    /// Stop after this many downloads.
    #[arg(long)]
    limit: Option<usize>,
    /// Seconds a load may take.
    #[arg(long, default_value_t = 60)]
    load_secs: u64,
    /// Only retest existing `not_working` files, renaming those that now load.
    #[arg(long)]
    recheck: bool,
    /// Productions checked in parallel, each with its own worker process.
    #[arg(long, default_value_t = 8)]
    jobs: usize,
    /// Run as a load-check worker, fed by the main process over stdin.
    #[arg(long, hide = true)]
    worker: bool,
}

/// One version of a release: a single file, or the disks of a set.
enum Version {
    File(&'static str),
    Disks(Vec<Vec<&'static str>>),
}

impl Version {
    fn name(&self, title: &str) -> Option<String> {
        match self {
            Version::File(url) => url_file_name(url),
            Version::Disks(disks) if disks.len() == 1 => {
                url_file_name(disks[0][0]).map(|n| adf_name(&n))
            }
            Version::Disks(_) => Some(format!("{}.zip", slug(title))),
        }
    }
}

fn slug(title: &str) -> String {
    title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn platform_tag(platform: &str) -> String {
    platform
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn adf_name(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("dms") => format!("{stem}.adf"),
        _ => name.to_string(),
    }
}

/// The fixed prefix for productions that are not run: graphics and music.
fn media_tag(file: &EmuFile) -> Option<&'static str> {
    file.categories.iter().find_map(|c| match *c {
        "Graphics" | "Photo" | "ASCII" | "ANSI" | "3D Model" => Some("gfx"),
        "Tracked Music" | "Streaming Music" | "Music" => Some("music"),
        _ => None,
    })
}

fn versions(urls: &[&'static str], media: bool) -> Vec<Version> {
    if media {
        return urls.iter().copied().map(Version::File).collect();
    }
    let mut out = vec![];
    let mut seen_disks = false;
    for d in release_downloads(urls) {
        match d {
            Download::File(url) => out.push(Version::File(url)),
            Download::Disks(disks) if !seen_disks => {
                seen_disks = true;
                out.push(Version::Disks(disks));
            }
            Download::Disks(_) => {}
        }
    }
    out
}

/// A `--worker` child process that load-checks one file at a time.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    replies: mpsc::Receiver<String>,
}

impl Worker {
    fn spawn(db: &Path) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new(std::env::current_exe()?);
        cmd.arg("--worker")
            .arg("--db")
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let (tx, replies) = mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Worker {
            child,
            stdin,
            replies,
        })
    }

    fn check(&mut self, id: &str, platform: &str, path: &Path, timeout: Duration) -> Result<Load> {
        writeln!(self.stdin, "{id}\t{platform}\t{}", path.display())?;
        self.stdin.flush()?;
        let reply = self.replies.recv_timeout(timeout)?;
        Ok(match reply.split_once('\t') {
            Some(("crash", core)) => Load::Crash(core.to_string()),
            _ if reply == "1" => Load::Ok,
            _ => Load::Fail,
        })
    }

    /// Kill the worker and everything it started (wine, gamescope).
    fn kill(mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

enum Load {
    Ok,
    Fail,
    /// The core process died; holds the core's path.
    Crash(String),
}

struct Mirror {
    args: Args,
    out: Mutex<Box<dyn Write + Send>>,
    downloads: AtomicUsize,
    cached: AtomicUsize,
}

impl Mirror {
    fn log(&self, line: &str) {
        let _ = writeln!(self.out.lock().unwrap(), "{line}");
    }

    fn limit_reached(&self) -> bool {
        self.args
            .limit
            .is_some_and(|l| self.downloads.load(Ordering::Relaxed) >= l)
    }

    /// Fetch `url` into `dir`, from the cache when it holds it.
    fn fetch(&self, url: &str, dir: &Path) -> Result<PathBuf> {
        let name = url_file_name(url).context("url has no file name")?;
        let dest = dir.join(&name);
        match cached_download(&cache_key(url)) {
            Some(cached) if cached.is_file() => {
                self.cached.fetch_add(1, Ordering::Relaxed);
                std::fs::copy(&cached, &dest)?;
            }
            _ => {
                if self.limit_reached() {
                    bail!("download limit reached");
                }
                self.downloads.fetch_add(1, Ordering::Relaxed);
                download_uncached(&cache_key(url), &dest)?;
            }
        }
        if release_io::dms::is_dms(&dest) {
            let adf = dest.with_file_name(adf_name(&name));
            release_io::dms::unpack(&dest, &adf)?;
            std::fs::remove_file(&dest)?;
            return Ok(adf);
        }
        Ok(dest)
    }

    /// Fetch a version into `dir`: the file itself, or a directory of disks.
    fn fetch_version(&self, version: &Version, dir: &Path) -> Result<PathBuf> {
        match version {
            Version::File(url) => self.fetch(url, dir),
            Version::Disks(disks) => {
                let set = dir.join("disks");
                std::fs::create_dir_all(&set)?;
                for alternatives in disks {
                    let mut last = None;
                    for url in alternatives {
                        match self.fetch(url, &set) {
                            Ok(_) => {
                                last = None;
                                break;
                            }
                            Err(e) => last = Some(e),
                        }
                    }
                    if let Some(e) = last {
                        return Err(e);
                    }
                }
                if disks.len() == 1 {
                    let file = std::fs::read_dir(&set)?.next().context("no disk")??.path();
                    return Ok(file);
                }
                Ok(set)
            }
        }
    }

    /// The first of the production's platforms that loads `path`.
    fn find_platform(
        &self,
        worker: &mut Option<Worker>,
        file: &EmuFile,
        path: &Path,
    ) -> Result<Option<&'static str>> {
        let id = file.get_meta("id");
        let timeout = Duration::from_secs(self.args.load_secs);
        for platform in file.platforms.iter().copied() {
            if worker.is_none() {
                *worker = Some(Worker::spawn(&self.args.db)?);
            }
            let w = worker.as_mut().unwrap();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let title = &file.game_info.title;
            match w.check(id, platform, path, timeout) {
                Ok(Load::Ok) => return Ok(Some(platform)),
                Ok(Load::Fail) => {}
                Ok(Load::Crash(core)) => {
                    self.log(&format!("{id} {title}: crash: {name} on {platform} ({core})"));
                }
                Err(e) => {
                    if matches!(
                        e.downcast_ref::<mpsc::RecvTimeoutError>(),
                        Some(mpsc::RecvTimeoutError::Disconnected)
                    ) {
                        let systems = newsys::platform_systems(platform).join("/");
                        self.log(&format!(
                            "{id} {title}: worker crash: {name} on {platform} ({systems})"
                        ));
                    }
                    worker.take().unwrap().kill();
                }
            }
        }
        Ok(None)
    }

    fn recheck(
        &self,
        worker: &mut Option<Worker>,
        file: &EmuFile, dir: &Path, existing: &[String]) -> Result<String> {
        let mut results = vec![];
        for entry in existing {
            let Some(name) = entry.strip_prefix("not_working-") else {
                continue;
            };
            let tmp = tempfile::tempdir()?;
            let path = tmp.path().join(name);
            std::fs::copy(dir.join(entry), &path)?;
            if let Some(platform) = self.find_platform(worker, file, &path)? {
                let target = format!("{}-{name}", platform_tag(platform));
                std::fs::rename(dir.join(entry), dir.join(&target))?;
                results.push(target);
            } else {
                results.push(entry.clone());
            }
        }
        if results.is_empty() {
            return Ok("(skip)".into());
        }
        Ok(results.join(", "))
    }

    fn mirror(&self, worker: &mut Option<Worker>, file: &EmuFile) -> Result<String> {
        let id = file.get_meta("id");
        let FileSource::Url(urls) = &file.path else {
            return Ok("no downloads".into());
        };
        let dir = self.args.out.join("demozoo").join(id);
        let existing: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        if self.args.recheck {
            return self.recheck(worker, file, &dir, &existing);
        }
        let title = file.game_info.title;
        let media = media_tag(file);
        let mut names = std::collections::HashSet::new();
        let todo: Vec<(Version, String)> = versions(urls.as_slice(), media.is_some())
            .into_iter()
            .filter_map(|v| Some((v.name(title)?, v)))
            .filter(|(name, _)| names.insert(name.to_lowercase()))
            .filter(|(name, _)| {
                let suffix = format!("-{}", name.to_lowercase());
                !existing.iter().any(|e| e.to_lowercase().ends_with(&suffix))
            })
            .map(|(name, v)| (v, name))
            .collect();
        if todo.is_empty() {
            return Ok("(skip)".into());
        }
        let mut results = vec![];
        for (version, name) in todo {
            if self.limit_reached() {
                break;
            }
            let tmp = tempfile::tempdir()?;
            let path = match self.fetch_version(&version, tmp.path()) {
                Ok(p) => p,
                Err(e) => {
                    results.push(format!("{name}: {e:#}"));
                    continue;
                }
            };
            let tag = match media {
                Some(tag) => tag.to_string(),
                None => self
                    .find_platform(worker, file, &path)?
                    .map_or_else(|| "not_working".to_string(), platform_tag),
            };
            std::fs::create_dir_all(&dir)?;
            let target = dir.join(format!("{tag}-{name}"));
            if path.is_dir() {
                zip_dir(&path, &target)?;
            } else {
                std::fs::copy(&path, &target)?;
            }
            results.push(format!("{tag}-{name}"));
        }
        Ok(results.join(", "))
    }
}

fn zip_dir(dir: &Path, target: &Path) -> Result<()> {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(target)?);
    let opts = zip::write::SimpleFileOptions::default();
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        zip.start_file(path.file_name().unwrap().to_string_lossy(), opts)?;
        zip.write_all(&std::fs::read(&path)?)?;
    }
    zip.finish()?;
    Ok(())
}

/// Send fds 1 and 2 (core and wine noise) to /dev/null and return a handle on
/// the real stdout.
#[cfg(unix)]
fn quiet_stdout() -> std::fs::File {
    use std::os::fd::FromRawFd;
    unsafe {
        let out = libc::dup(1);
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        libc::dup2(null, 1);
        if std::env::var_os("RUST_LOG").is_none() {
            libc::dup2(null, 2);
        }
        libc::close(null);
        std::fs::File::from_raw_fd(out)
    }
}

/// Does `platform`'s own system load `path`?
fn check_load(file: &EmuFile, platform: &str, path: &Path, over: Option<&Override>) -> Load {
    let mut meta: std::collections::HashMap<String, String> = file
        .meta_strings()
        .map(|(k, v)| (k.to_string(), v.into_owned()))
        .collect();
    meta.insert("platform".into(), platform.to_string());
    meta.insert("use_proc".into(), "1".into());
    let Ok(wf) = release_io::unpack_release(path, &meta) else {
        return Load::Fail;
    };
    let opts = SysOpts {
        latency: 2,
        silent_drive: true,
        ..Default::default()
    };
    let expected = newsys::platform_systems(platform);
    match NewSys::new(&opts).load_prepared(wf, over) {
        Ok(r) if expected.is_empty() || expected.contains(&r.system_name) => Load::Ok,
        Ok(_) => Load::Fail,
        Err(e) => {
            tracing::warn!("{platform}: {e:#}");
            #[cfg(unix)]
            if let Some(c) = e.downcast_ref::<newsys::retro_emu::CoreCrashed>() {
                return Load::Crash(c.core.display().to_string());
            }
            Load::Fail
        }
    }
}

/// Answer `<id>\t<platform>\t<path>` lines on stdin with `1`, `0` or
/// `crash\t<core>`.
fn worker(files: &[EmuFile], out: &mut impl Write) -> Result<()> {
    let by_id: std::collections::HashMap<&str, &EmuFile> =
        files.iter().map(|f| (f.get_meta("id"), f)).collect();
    let overrides = overrides::load_default(|_| Some(newsys::libretro::RETROK_UNKNOWN))?;
    for line in std::io::stdin().lock().lines() {
        let line = line?;
        let mut parts = line.splitn(3, '\t');
        let (Some(id), Some(platform), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            bail!("bad request: {line}");
        };
        let load = by_id.get(id).map_or(Load::Fail, |file| {
            let over = file.demo_id().and_then(|d| overrides.get(&d));
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                check_load(file, platform, Path::new(path), over)
            }))
            .unwrap_or(Load::Fail)
        });
        match load {
            Load::Ok => writeln!(out, "1")?,
            Load::Fail => writeln!(out, "0")?,
            Load::Crash(core) => writeln!(out, "crash\t{core}")?,
        }
        out.flush()?;
    }
    Ok(())
}

fn main() -> Result<()> {
    #[cfg(unix)]
    if let Some(code) = newsys::retro_emu::process_worker_main() {
        std::process::exit(code);
    }
    let mut args = Args::parse();
    #[cfg(target_os = "linux")]
    newsys::wine::add_wine_to_path();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("error")),
        )
        .with_writer(std::io::stderr)
        .init();
    #[cfg(unix)]
    let mut out = quiet_stdout();
    #[cfg(not(unix))]
    let mut out = std::io::stdout();

    args.db = std::fs::canonicalize(&args.db)?;
    let filter = DbFilter {
        include: &[],
        exclude: &[],
    };
    let mut files = vec![];
    collect_db(&args.db, &filter, &mut files)?;
    if args.worker {
        return worker(&files, &mut out);
    }
    std::fs::create_dir_all(&args.out)?;
    args.out = std::fs::canonicalize(&args.out)?;

    let mirror = Mirror {
        downloads: AtomicUsize::new(0),
        cached: AtomicUsize::new(0),
        args,
        out: Mutex::new(Box::new(out)),
    };
    let todo = Mutex::new(files.iter().filter(|file| {
        let id = file.get_meta("id");
        mirror.args.id.is_empty() || mirror.args.id.iter().any(|w| w == id)
    }));
    std::thread::scope(|scope| {
        for _ in 0..mirror.args.jobs.max(1) {
            scope.spawn(|| {
                let mut worker = None;
                loop {
                    if mirror.limit_reached() {
                        break;
                    }
                    let Some(file) = todo.lock().unwrap().next() else {
                        break;
                    };
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        mirror.mirror(&mut worker, file)
                    }))
                    .unwrap_or_else(|_| Ok("panicked".into()));
                    let line = result.unwrap_or_else(|e| format!("error: {e:#}"));
                    mirror.log(&format!(
                        "{} {}: {line}",
                        file.get_meta("id"),
                        file.game_info.title
                    ));
                }
                if let Some(w) = worker {
                    w.kill();
                }
            });
        }
    });
    mirror.log(&format!(
        "{} files downloaded, {} from cache",
        mirror.downloads.load(Ordering::Relaxed),
        mirror.cached.load(Ordering::Relaxed)
    ));
    Ok(())
}
