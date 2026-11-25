//! Mirrors demozoo productions into `<out>/demozoo/<id>/<platform>-<filename>`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use database::emu_file::{Download, EmuFile, FileSource, cache_key, release_downloads, url_file_name};
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
    #[arg(long, default_value = "mirror")]
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
            Version::Disks(disks) if disks.len() == 1 => url_file_name(disks[0][0]).map(|n| adf_name(&n)),
            Version::Disks(_) => Some(format!("{}.zip", slug(title))),
        }
    }
}

fn slug(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

fn platform_tag(platform: &str) -> String {
    platform
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
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

struct Mirror {
    args: Args,
    downloads: usize,
    overrides: std::collections::HashMap<database::emu_file::DemoId, Override>,
}

impl Mirror {
    fn limit_reached(&self) -> bool {
        self.args.limit.is_some_and(|l| self.downloads >= l)
    }

    /// Fetch `url` into `dir`, from the cache when it holds it.
    fn fetch(&mut self, url: &str, dir: &Path) -> Result<PathBuf> {
        let name = url_file_name(url).context("url has no file name")?;
        let dest = dir.join(&name);
        match cached_download(&cache_key(url)) {
            Some(cached) if cached.is_file() => {
                std::fs::copy(&cached, &dest)?;
            }
            _ => {
                if self.limit_reached() {
                    bail!("download limit reached");
                }
                self.downloads += 1;
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
    fn fetch_version(&mut self, version: &Version, dir: &Path) -> Result<PathBuf> {
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
    fn find_platform(&self, file: &EmuFile, path: &Path) -> Option<&'static str> {
        let over = file.demo_id().and_then(|d| self.overrides.get(&d)).cloned();
        file.platforms.iter().copied().find(|platform| {
            let mut meta: std::collections::HashMap<String, String> = file
                .meta_strings()
                .map(|(k, v)| (k.to_string(), v.into_owned()))
                .collect();
            meta.insert("platform".into(), platform.to_string());
            let Ok(wf) = release_io::unpack_release(path, &meta) else {
                return false;
            };
            let over = over.clone();
            let name: &'static str = platform;
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let opts = SysOpts {
                    latency: 2,
                    silent_drive: true,
                    ..Default::default()
                };
                let expected = newsys::platform_systems(name);
                let ok = NewSys::new(&opts)
                    .load_prepared(wf, over.as_ref())
                    .is_ok_and(|r| expected.is_empty() || expected.contains(&r.system_name));
                let _ = tx.send(ok);
            });
            rx.recv_timeout(Duration::from_secs(self.args.load_secs))
                .unwrap_or(false)
        })
    }

    fn mirror(&mut self, file: &EmuFile) -> Result<String> {
        let id = file.get_meta("id");
        let FileSource::Url(urls) = &file.path else {
            return Ok("no downloads".into());
        };
        let dir = self.args.out.join("demozoo").join(id);
        let existing: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
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
                    .find_platform(file, &path)
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
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.flatten().map(|e| e.path()).collect();
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

fn main() -> Result<()> {
    let mut args = Args::parse();
    std::fs::create_dir_all(&args.out)?;
    args.out = std::fs::canonicalize(&args.out)?;
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

    let filter = DbFilter {
        include: &[],
        exclude: &[],
    };
    let mut files = vec![];
    collect_db(&args.db, &filter, &mut files)?;
    let mut mirror = Mirror {
        overrides: overrides::load_default(|_| Some(newsys::libretro::RETROK_UNKNOWN))?,
        downloads: 0,
        args,
    };
    for file in &files {
        let id = file.get_meta("id");
        if !mirror.args.id.is_empty() && !mirror.args.id.iter().any(|w| w == id) {
            continue;
        }
        if mirror.limit_reached() {
            break;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mirror.mirror(file)))
            .unwrap_or_else(|_| Ok("panicked".into()));
        let line = result.unwrap_or_else(|e| format!("error: {e:#}"));
        writeln!(out, "{id} {}: {line}", file.game_info.title)?;
    }
    Ok(())
}
