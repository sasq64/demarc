//! Video file backend: decodes with the `ffmpeg` executable, one process for
//! the picture and one for the sound, each piped back as raw samples.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;

use anyhow::{Context, Result, bail};
use tracing::warn;

use crate::backend::{Backend, MediaCmd, MediaKind, VideoFrame};

const SAMPLE_RATE: u32 = 48000;

/// Codecs worth decoding on the GPU; NVDEC gets MPEG-4 ASP wrong.
const HWACCEL_CODECS: &[&str] = &["h264", "hevc", "vp9", "av1"];

/// Subtitle codecs that are pictures, drawn with `overlay` instead of libass.
const IMAGE_SUB_CODECS: &[&str] = &["hdmv_pgs_subtitle", "dvd_subtitle", "dvb_subtitle", "xsub"];

const SUB_EXTENSIONS: &[&str] = &["srt", "ass", "ssa", "vtt"];

/// Bytes of audio read from the pipe at a time.
const AUDIO_CHUNK: usize = 4096;

/// Whether both `ffmpeg` and `ffprobe` can be run.
pub fn is_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(|| {
        ["ffmpeg", "ffprobe"].iter().all(|exe| {
            Command::new(exe)
                .arg("-version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
    })
}

#[derive(Debug, PartialEq)]
pub struct VideoInfo {
    pub width: usize,
    pub height: usize,
    pub fps: f64,
    /// Pixel aspect ratio.
    pub sar: f64,
    pub codec: String,
}

/// `30000/1001` or `16:9` as a number; `None` for `0/0`, `N/A` and the like.
fn parse_ratio(s: &str) -> Option<f64> {
    let (n, d) = s.split_once(['/', ':']).unwrap_or((s, "1"));
    let (n, d) = (n.trim().parse::<f64>().ok()?, d.trim().parse::<f64>().ok()?);
    (n > 0.0 && d > 0.0).then_some(n / d)
}

/// Parse `ffprobe -of default=nw=1` output for one video stream.
pub fn parse_probe(text: &str) -> Result<VideoInfo> {
    let fields: HashMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
    let dim = |key| -> Result<usize> {
        fields
            .get(key)
            .and_then(|v| v.trim().parse().ok())
            .filter(|&v| v > 0)
            .with_context(|| format!("ffprobe gave no {key}"))
    };
    let fps = ["avg_frame_rate", "r_frame_rate"]
        .iter()
        .find_map(|key| fields.get(key).and_then(|v| parse_ratio(v)))
        .unwrap_or(25.0);
    Ok(VideoInfo {
        width: dim("width")?,
        height: dim("height")?,
        fps,
        sar: fields
            .get("sample_aspect_ratio")
            .and_then(|v| parse_ratio(v))
            .unwrap_or(1.0),
        codec: fields.get("codec_name").map_or("", |v| v.trim()).to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Subtitle {
    File(PathBuf),
    /// Index among the file's subtitle streams.
    Text(usize),
    Image(usize),
}

/// `key=value` fields of one `ffprobe -of compact=p=0` line, which escapes
/// `|` and `\` with a backslash.
fn compact_fields(line: &str) -> HashMap<String, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => field.extend(chars.next()),
            '|' => fields.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    fields
        .iter()
        .filter_map(|f| f.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

/// Subtitle streams and their names, one per line as listed by `ffprobe -of
/// compact=p=0 -show_entries stream=codec_name:stream_tags=language,title`.
pub fn parse_subtitle_streams(text: &str) -> Vec<(Subtitle, String)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            let f = compact_fields(line.trim());
            let get = |k: &str| f.get(k).map(String::as_str).filter(|v| !v.is_empty());
            let codec = get("codec_name").unwrap_or("");
            let name = match (get("tag:title"), get("tag:language")) {
                (Some(t), Some(l)) => format!("{t} ({l})"),
                (Some(n), None) | (None, Some(n)) => n.to_owned(),
                (None, None) => format!("Track {} ({codec})", i + 1),
            };
            let sub = if IMAGE_SUB_CODECS.contains(&codec) {
                Subtitle::Image(i)
            } else {
                Subtitle::Text(i)
            };
            (sub, name)
        })
        .collect()
}

/// `movie.srt`, `movie.en.srt` and the like next to `movie.mkv`.
fn external_subtitles(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let ext_ok = p
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| SUB_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            ext_ok && name.strip_prefix(stem).is_some_and(|rest| rest.starts_with('.'))
        })
        .collect();
    found.sort();
    found
}

fn subtitle_tracks(path: &Path) -> Vec<(Subtitle, String)> {
    let mut tracks: Vec<(Subtitle, String)> = external_subtitles(path)
        .into_iter()
        .map(|p| {
            let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            (Subtitle::File(p), name)
        })
        .collect();
    let streams = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "s", "-show_entries"])
        .arg("stream=codec_name:stream_tags=language,title")
        .args(["-of", "compact=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output();
    if let Ok(out) = streams {
        tracks.extend(parse_subtitle_streams(&String::from_utf8_lossy(&out.stdout)));
    }
    tracks
}

/// Escape a path for use as a filter option inside a filtergraph: once for
/// the option value, once more for the graph.
fn escape_filter_arg(s: &str) -> String {
    let escape = |s: &str, special: &str| {
        s.chars().fold(String::new(), |mut out, c| {
            if special.contains(c) {
                out.push('\\');
            }
            out.push(c);
            out
        })
    };
    escape(&escape(s, "\\':"), "\\'[],;")
}

/// Output arguments for the picture process. Input seeking restarts the
/// timestamps at zero, so libass gets them shifted back by `pos`.
pub fn video_output_args(path: &Path, sub: Option<&Subtitle>, pos: &str) -> Vec<String> {
    let libass = |file: &Path, extra: String| {
        let file = escape_filter_arg(&file.to_string_lossy());
        vec![
            "-map".into(),
            "0:v:0".into(),
            "-vf".into(),
            format!("setpts=PTS+{pos}/TB,subtitles={file}{extra},setpts=PTS-STARTPTS"),
        ]
    };
    let mut args = match sub {
        None => vec!["-map".into(), "0:v:0".into()],
        Some(Subtitle::File(file)) => libass(file, String::new()),
        Some(Subtitle::Text(i)) => libass(path, format!(":si={i}")),
        Some(Subtitle::Image(i)) => vec![
            "-filter_complex".into(),
            format!("[0:v:0][0:s:{i}]overlay[v]"),
            "-map".into(),
            "[v]".into(),
        ],
    };
    args.extend(["-f", "rawvideo", "-pix_fmt", "rgba"].map(String::from));
    args
}

fn ffprobe(path: &Path, stream: &str, entries: &str) -> Result<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries"])
        .arg(entries)
        .args(["-of", "default=nw=1"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .context("Could not run ffprobe")?;
    if !out.status.success() {
        bail!(
            "ffprobe failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn spawn_ffmpeg(path: &Path, input: &[&str], output: &[&str]) -> Result<(Child, ChildStdout)> {
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin"])
        .args(input)
        .arg("-i")
        .arg(path)
        .args(output)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("Could not run ffmpeg")?;
    let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
    Ok((child, stdout))
}

struct Decoders {
    children: Vec<Child>,
    // Behind a lock only so the backend is `Sync`; reached through `get_mut`.
    video: Mutex<Receiver<Arc<Vec<u32>>>>,
    audio: Option<Mutex<Receiver<Vec<f32>>>>,
}

impl Drop for Decoders {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub struct FfmpegEmu {
    path: PathBuf,
    info: VideoInfo,
    has_audio: bool,
    decoders: Decoders,
    frame: Arc<Vec<u32>>,
    /// Decoded audio not yet handed out per frame.
    pending: Vec<f32>,
    audio: Vec<f32>,
    sample_debt: f64,
    ended: bool,
    /// Pictures shown since the start of the file.
    frames: u64,
    /// Seconds, when the container knows.
    length: Option<f64>,
    subtitles: Vec<(Subtitle, String)>,
    subtitle: Option<usize>,
}

impl FfmpegEmu {
    pub fn new(path: &Path) -> Result<Self> {
        let info = parse_probe(&ffprobe(
            path,
            "v:0",
            "stream=codec_name,width,height,avg_frame_rate,r_frame_rate,sample_aspect_ratio",
        )?)?;
        let has_audio = !ffprobe(path, "a:0", "stream=index")?.trim().is_empty();
        let length = ffprobe(path, "v:0", "format=duration")?
            .trim()
            .strip_prefix("duration=")
            .and_then(|d| d.parse::<f64>().ok())
            .filter(|d| *d > 0.0);
        let decoders = Self::start(path, &info, has_audio, None, 0.0)?;
        Ok(Self {
            path: path.to_owned(),
            frame: Arc::new(vec![u32::from_ne_bytes([0, 0, 0, 255]); info.width * info.height]),
            info,
            has_audio,
            decoders,
            pending: Vec::new(),
            audio: Vec::new(),
            sample_debt: 0.0,
            ended: false,
            frames: 0,
            length,
            subtitles: subtitle_tracks(path),
            subtitle: None,
        })
    }

    fn start(
        path: &Path,
        info: &VideoInfo,
        has_audio: bool,
        sub: Option<&Subtitle>,
        pos: f64,
    ) -> Result<Decoders> {
        let pos = format!("{pos:.3}");
        let mut input = vec!["-ss", &pos];
        if HWACCEL_CODECS.contains(&info.codec.as_str()) {
            input.extend(["-hwaccel", "auto"]);
        }
        let output = video_output_args(path, sub, &pos);
        let output: Vec<&str> = output.iter().map(String::as_str).collect();
        let (video_child, mut stdout) = spawn_ffmpeg(path, &input, &output)?;
        let mut children = vec![video_child];
        let pixels = info.width * info.height;
        let (tx, video) = sync_channel(4);
        thread::spawn(move || {
            loop {
                let mut frame = vec![0u32; pixels];
                // Safety: any bytes are a valid `u32`; rgba lands in the
                // `[r, g, b, a]` memory order `frame_bytes` expects.
                let bytes = unsafe {
                    std::slice::from_raw_parts_mut(frame.as_mut_ptr() as *mut u8, pixels * 4)
                };
                if stdout.read_exact(bytes).is_err() || tx.send(Arc::new(frame)).is_err() {
                    break;
                }
            }
        });

        let audio = if has_audio {
            let rate = SAMPLE_RATE.to_string();
            let (audio_child, mut stdout) = spawn_ffmpeg(
                path,
                &["-ss", &pos],
                &["-map", "0:a:0", "-f", "f32le", "-ac", "2", "-ar", &rate],
            )?;
            children.push(audio_child);
            let (tx, audio) = sync_channel(16);
            thread::spawn(move || {
                let mut buf = vec![0u8; AUDIO_CHUNK];
                loop {
                    let mut n = match stdout.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    // A read can stop mid-sample; finish it.
                    let rem = n % 4;
                    if rem != 0 {
                        if stdout.read_exact(&mut buf[n..n + 4 - rem]).is_err() {
                            break;
                        }
                        n += 4 - rem;
                    }
                    let samples = buf[..n]
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect();
                    if tx.send(samples).is_err() {
                        break;
                    }
                }
            });
            Some(Mutex::new(audio))
        } else {
            None
        };
        Ok(Decoders {
            children,
            video: Mutex::new(video),
            audio,
        })
    }

    /// Interleaved stereo samples that make up one frame of video.
    fn frame_samples(&mut self) -> usize {
        self.sample_debt += SAMPLE_RATE as f64 / self.info.fps;
        let n = self.sample_debt.floor();
        self.sample_debt -= n;
        n as usize * 2
    }

    /// Wait for the next picture, so the sound never runs ahead of or behind
    /// it; `false` if the decoder is gone.
    fn next_frame(&mut self) -> bool {
        let Ok(video) = self.decoders.video.get_mut() else {
            return false;
        };
        match video.recv() {
            Ok(frame) => {
                self.frame = frame;
                self.frames += 1;
            }
            Err(_) => self.ended = true,
        }
        true
    }

    /// Take one frame's worth of audio out of the decoder, or `None` once it
    /// has run dry.
    fn take_audio(&mut self) -> Option<Vec<f32>> {
        let want = self.frame_samples();
        let rx = self.decoders.audio.as_mut()?.get_mut().ok()?;
        while self.pending.len() < want {
            match rx.recv() {
                Ok(chunk) => self.pending.extend(chunk),
                Err(_) => break,
            }
        }
        let n = want.min(self.pending.len());
        Some(self.pending.drain(..n).collect())
    }

    fn seek_to(&mut self, pos: f64) {
        let sub = self.subtitle.map(|i| &self.subtitles[i].0);
        match Self::start(&self.path, &self.info, self.has_audio, sub, pos) {
            Ok(decoders) => self.decoders = decoders,
            Err(e) => {
                warn!("Could not restart ffmpeg: {e}");
                return;
            }
        }
        self.pending.clear();
        self.audio.clear();
        self.sample_debt = 0.0;
        self.ended = false;
        self.frames = (pos * self.info.fps).round() as u64;
    }
}

impl Backend for FfmpegEmu {
    fn run(&mut self) -> bool {
        if self.ended {
            return true;
        }
        if !self.next_frame() {
            return false;
        }
        if let Some(samples) = self.take_audio() {
            self.audio.extend(samples);
        }
        true
    }

    fn with_audio(&mut self, f: &mut dyn FnMut(&[f32])) {
        f(&self.audio);
        self.audio.clear();
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(self.info.width, self.info.height, &self.frame);
    }

    fn frame(&self) -> VideoFrame {
        VideoFrame {
            width: self.info.width,
            height: self.info.height,
            pixels: Arc::clone(&self.frame),
        }
    }

    fn get_frame_size(&self) -> (usize, usize) {
        (self.info.width, self.info.height)
    }

    fn aspect_ratio(&self) -> f32 {
        (self.info.width as f64 * self.info.sar / self.info.height as f64) as f32
    }

    fn sample_rate(&self) -> f64 {
        if self.has_audio {
            SAMPLE_RATE as f64
        } else {
            0.0
        }
    }

    fn fps(&self) -> f64 {
        self.info.fps
    }

    fn is_idle(&self) -> bool {
        self.ended
    }

    fn skip_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            if !self.next_frame() || self.ended {
                break;
            }
            self.take_audio();
        }
    }

    fn reset(&mut self) {
        self.seek_to(0.0);
    }

    fn media_kind(&self) -> Option<MediaKind> {
        Some(MediaKind::Video)
    }

    fn position(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs_f64(self.frames as f64 / self.info.fps))
    }

    fn length(&self) -> Option<std::time::Duration> {
        self.length.map(std::time::Duration::from_secs_f64)
    }

    fn media(&mut self, cmd: MediaCmd) {
        let now = self.frames as f64 / self.info.fps;
        match cmd {
            MediaCmd::Seek(secs) => self.seek_to((now + secs as f64).max(0.0)),
            MediaCmd::NextSubtitle if !self.subtitles.is_empty() => {
                self.subtitle = match self.subtitle {
                    None => Some(0),
                    Some(i) if i + 1 < self.subtitles.len() => Some(i + 1),
                    Some(_) => None,
                };
                self.seek_to(now);
            }
            _ => {}
        }
    }

    fn subtitle_name(&self) -> Option<String> {
        if self.subtitles.is_empty() {
            return None;
        }
        Some(self.subtitle.map_or("None".into(), |i| self.subtitles[i].1.clone()))
    }

    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        1
    }
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}

    fn get_info(&self) -> Option<String> {
        Some(format!(
            "Video {}x{} @ {:.2} fps",
            self.info.width, self.info.height, self.info.fps
        ))
    }
}

#[cfg(test)]
#[path = "tests/ffmpeg_emu_tests.rs"]
mod tests;
