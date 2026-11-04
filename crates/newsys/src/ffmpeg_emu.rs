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

fn ffprobe(path: &Path, stream: &str, entries: &str) -> Result<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", stream, "-show_entries"])
        .arg(format!("stream={entries}"))
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
}

impl FfmpegEmu {
    pub fn new(path: &Path) -> Result<Self> {
        let info = parse_probe(&ffprobe(
            path,
            "v:0",
            "codec_name,width,height,avg_frame_rate,r_frame_rate,sample_aspect_ratio",
        )?)?;
        let has_audio = !ffprobe(path, "a:0", "index")?.trim().is_empty();
        let decoders = Self::start(path, &info, has_audio, 0.0)?;
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
        })
    }

    fn start(path: &Path, info: &VideoInfo, has_audio: bool, pos: f64) -> Result<Decoders> {
        let pos = format!("{pos:.3}");
        let mut input = vec!["-ss", &pos];
        if HWACCEL_CODECS.contains(&info.codec.as_str()) {
            input.extend(["-hwaccel", "auto"]);
        }
        let (video_child, mut stdout) = spawn_ffmpeg(
            path,
            &input,
            &["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "rgba"],
        )?;
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
        match Self::start(&self.path, &self.info, self.has_audio, pos) {
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

    fn media(&mut self, cmd: MediaCmd) {
        if let MediaCmd::Seek(secs) = cmd {
            let pos = self.frames as f64 / self.info.fps + secs as f64;
            self.seek_to(pos.max(0.0));
        }
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
