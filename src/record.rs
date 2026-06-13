//! Records the first emulator's output to an MP4 with `ffmpeg`.
//!
//! ## Capturing "after HUD + post-process"
//!
//! Per-frame screenshots of the primary window come back black in this app's
//! render graph, so instead we render the whole composited scene into an
//! intermediate image and read *that* back every frame:
//!
//! 1. [`setup_capture`] creates a window-sized render-target image, retargets
//!    every existing camera (the emulator post-process cameras *and* the HUD/UI
//!    camera) to it, and adds a small display camera that draws the image back
//!    onto the window so the user still sees everything.
//! 2. A [`Readback`] component on that image streams its pixels to the CPU
//!    every frame (the engine's continuous-readback primitive), firing
//!    [`on_readback`].
//!
//! ## Encoding
//!
//! * **Never block emulation.** Readback bytes are pushed over an unbounded
//!   channel to a pump thread; the render loop never waits on `ffmpeg`. Row
//!   unpadding happens on the pump thread.
//! * **Survive an unclean exit.** A single `ffmpeg` writes *directly* to the
//!   output as a fragmented MP4 (`+frag_keyframe+empty_moov`) with packet
//!   flushing, so the file stays playable up to the last flushed fragment even
//!   if the process is killed; a clean quit finalizes it normally.
//! * **Cheap to encode.** `libx264 -preset ultrafast` keeps the encoder ahead
//!   of capture at the cost of a larger file — the intended trade-off.
//!
//! Audio is the first emulator's **played** stream — the cpal output, tapped at
//! the device callback — muxed into the same `ffmpeg` live through a FIFO
//! (Unix); on platforms without FIFOs it records video only. We deliberately
//! record the played stream rather than the core's raw output: the emulator
//! emits raw audio per `core.run()` at emulated-time pace, bursting many frames
//! at once during startup catch-up / audio-buffer fill, whereas the cpal output
//! has already been resampled and rate-controlled to a smooth, realtime pace by
//! the audio sink. Recording that means the audio matches the realtime video
//! frame-for-frame with no re-pacing, so it neither drifts nor glitches.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, Sender, channel};

use bevy::app::AppExit;
use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::camera::visibility::RenderLayers;
use bevy::prelude::*;
use bevy::render::gpu_readback::{Readback, ReadbackComplete};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::window::PrimaryWindow;

use crate::emulator::Emulator;

/// Stereo audio samples and the native rate they were captured at, sent from
/// the emulator's audio tap to the recorder.
pub type AudioChunk = (f32, Vec<i16>);

/// Render layer used only by the on-screen display camera + sprite, so they
/// don't intersect the emulator (layer 1) or HUD (layer 2) cameras.
const DISPLAY_LAYER: usize = 3;

/// If no audio has arrived after this many captured frames, start encoding
/// video only rather than waiting forever for a (possibly silent) core.
const START_AUDIO_WAIT_FRAMES: u32 = 60;

/// Cap on audio buffered before the encoder starts (~2s of 48kHz stereo), so a
/// late encoder start can't grow memory unbounded. Trimmed on whole stereo
/// `i16` frames (4 bytes).
const MAX_PENDING_AUDIO: usize = 48_000 * 2 * 2 * 2;

/// Video frames of audio to keep when the encoder starts. Audio (the cpal
/// stream) begins playing as soon as the emulator is active, but video isn't
/// captured until the window has sized up and the first readback lands, so the
/// buffer holds a startup backlog with no matching frames. We drop all but the
/// newest few frames so the audio starts roughly on the first captured scene
/// instead of ahead of it.
const AUDIO_START_LATENCY_FRAMES: usize = 3;

#[derive(Resource)]
pub struct Recorder {
    /// Final MP4 path requested by the user; `ffmpeg` writes it directly.
    output: PathBuf,
    /// Frame rate handed to `ffmpeg`, from the `--record-fps` argument. One
    /// frame is captured per rendered frame, so this must match the screen/
    /// render rate or the video stretches out of sync with the realtime audio.
    fps: u32,
    /// Set once the capture target + readback have been wired up.
    capture_ready: bool,
    /// Dimensions of the capture image (== window physical size).
    width: u32,
    height: u32,
    /// `ffmpeg`'s `-pixel_format` for the readback bytes, derived from the
    /// capture image's texture format in [`setup_capture`].
    pixel_format: &'static str,
    /// Number of video frames handed to the encoder (for the finish log).
    frames_sent: u64,
    /// Native audio rate seen from the core (0 until the first chunk arrives).
    audio_rate: f32,
    /// Audio captured before the encoder starts, as interleaved s16le bytes.
    pending_audio: Vec<u8>,
    /// Frames seen while still waiting for an audio rate before starting.
    frames_waited: u32,
    /// The running encoder, started lazily once the audio rate is known.
    pipeline: Option<Pipeline>,
    /// Cloned to the first emulator so it can push its audio here.
    audio_in_tx: Sender<AudioChunk>,
    /// `Mutex` only to satisfy `Sync` for the resource; accessed via `get_mut`.
    audio_in_rx: Mutex<Receiver<AudioChunk>>,
    finished: bool,
}

impl Recorder {
    pub fn new(output: PathBuf, fps: u32) -> Self {
        let (audio_in_tx, audio_in_rx) = channel();
        Self {
            output,
            fps: fps.max(1),
            capture_ready: false,
            width: 0,
            height: 0,
            pixel_format: "rgba",
            frames_sent: 0,
            audio_rate: 0.0,
            pending_audio: Vec::new(),
            frames_waited: 0,
            pipeline: None,
            audio_in_tx,
            audio_in_rx: Mutex::new(audio_in_rx),
            finished: false,
        }
    }

    /// A sender the emulator can use to feed its audio into the recording.
    pub fn audio_sender(&self) -> Sender<AudioChunk> {
        self.audio_in_tx.clone()
    }

    /// Feed one read-back frame (BGRA, possibly row-padded) to the encoder,
    /// starting `ffmpeg` lazily once the audio rate is known (or after a short
    /// grace period for silent cores).
    fn push_video(&mut self, data: Vec<u8>) {
        if self.finished {
            return;
        }
        if self.pipeline.is_none() {
            if self.audio_rate <= 0.0 && self.frames_waited < START_AUDIO_WAIT_FRAMES {
                self.frames_waited += 1;
                return;
            }
            self.start_pipeline();
            if self.pipeline.is_none() {
                return; // start failed; recording disabled
            }
        }
        if let Some(p) = &self.pipeline {
            // Unbounded send: never blocks the render loop on the encoder.
            if p.video_tx.send(data).is_ok() {
                self.frames_sent += 1;
            }
        }
    }

    /// Forward captured audio to the encoder, or buffer it until the encoder
    /// starts, tracking the native rate for `ffmpeg`'s input. The audio comes
    /// from the cpal output (the actual played stream), which is already
    /// realtime-paced, so it can be muxed straight through without re-pacing.
    fn write_audio(&mut self, rate: f32, samples: &[i16]) {
        if self.finished {
            return;
        }
        if rate > 0.0 {
            self.audio_rate = rate;
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        if let Some(p) = &self.pipeline {
            if let Some(tx) = &p.audio_tx {
                let _ = tx.send(bytes);
            }
            return; // video-only pipeline: discard audio
        }
        self.pending_audio.extend_from_slice(&bytes);
        if self.pending_audio.len() > MAX_PENDING_AUDIO {
            let trim = (self.pending_audio.len() - MAX_PENDING_AUDIO).next_multiple_of(4);
            self.pending_audio
                .drain(..trim.min(self.pending_audio.len()));
        }
    }

    /// Spawn `ffmpeg` and its pump threads. Video goes in over stdin (BGRA);
    /// audio (if any) over a FIFO so a single process muxes both to the output.
    fn start_pipeline(&mut self) {
        let (w, h, fps) = (self.width, self.height, self.fps);
        let want_audio = self.audio_rate > 0.0;

        // Create the audio FIFO (Unix only). Falls back to video-only on
        // failure or on platforms without FIFOs.
        let mut fifo_path: Option<PathBuf> = None;
        #[cfg(unix)]
        if want_audio {
            let path = std::env::temp_dir().join(format!("demarc-rec-{}.pcm", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let made = Command::new("mkfifo")
                .arg(&path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if made {
                fifo_path = Some(path);
            } else {
                warn!("record: could not create audio FIFO; recording video only");
            }
        }

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            // Video input (stdin): unpadded readback bytes from the pump thread.
            .args(["-f", "rawvideo", "-pixel_format", self.pixel_format])
            .arg("-video_size")
            .arg(format!("{w}x{h}"))
            .arg("-framerate")
            .arg(fps.to_string())
            .args(["-i", "pipe:0"]);
        if let Some(fifo) = &fifo_path {
            cmd.args(["-f", "s16le", "-ar"])
                .arg((self.audio_rate.round() as u32).to_string())
                .args(["-ac", "2", "-i"])
                .arg(fifo);
        }
        cmd.args(["-map", "0:v:0"]);
        if fifo_path.is_some() {
            cmd.args(["-map", "1:a:0"]);
        }
        // ultrafast: keep the encoder comfortably ahead of capture. `-g fps`
        // makes a keyframe (and thus a flushable fragment) ~once per second.
        cmd.args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg("-g")
        .arg(fps.to_string())
        // H.264/yuv420p needs even dimensions; pad up if odd-sized.
        .args(["-vf", "pad=ceil(iw/2)*2:ceil(ih/2)*2"]);
        if fifo_path.is_some() {
            cmd.args(["-c:a", "aac", "-b:a", "192k"]);
        }
        // Fragmented MP4 + packet flushing: the file stays valid/playable as it
        // grows, so quitting (or crashing) doesn't leave an empty container.
        cmd.args([
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
            "-flush_packets",
            "1",
        ])
        .arg(&self.output)
        .stdin(Stdio::piped())
        .stdout(Stdio::null());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                error!("record: failed to start ffmpeg ({e}); is it installed?");
                self.finished = true;
                if let Some(f) = &fifo_path {
                    let _ = std::fs::remove_file(f);
                }
                return;
            }
        };

        // Video pump: strips any per-row padding from the readback and writes
        // tightly-packed BGRA to ffmpeg's stdin on its own thread, so neither
        // the unpadding nor a slow encoder ever stalls the render loop.
        let mut stdin = child.stdin.take().expect("piped stdin");
        let (video_tx, video_rx) = channel::<Vec<u8>>();
        let rows = h as usize;
        let tight_row = w as usize * 4;
        let _ = std::thread::Builder::new()
            .name("record-video".into())
            .spawn(move || {
                while let Ok(data) = video_rx.recv() {
                    if rows == 0 || data.len() < tight_row * rows {
                        continue;
                    }
                    let padded_row = data.len() / rows;
                    let res = if padded_row == tight_row {
                        stdin.write_all(&data)
                    } else {
                        // GPU readback pads each row up to 256 bytes; emit only
                        // the meaningful `tight_row` bytes of each.
                        let mut r = Ok(());
                        for y in 0..rows {
                            let off = y * padded_row;
                            r = stdin.write_all(&data[off..off + tight_row]);
                            if r.is_err() {
                                break;
                            }
                        }
                        r
                    };
                    if res.is_err() {
                        break; // ffmpeg went away
                    }
                }
                // Dropping `stdin` here closes the pipe (EOF for ffmpeg).
            });

        // Audio pump: opens the FIFO for writing (rendezvous with ffmpeg's read
        // side) and forwards PCM chunks. Detached — never joined — so a stuck
        // open can't hang shutdown.
        let mut audio_tx = None;
        if let Some(fifo) = &fifo_path {
            let (atx, arx) = channel::<Vec<u8>>();
            let fifo = fifo.clone();
            // Drop the startup backlog so the audio doesn't begin ahead of the
            // first captured video frame; keep only the newest few frames.
            let frame_bytes = ((self.audio_rate / fps as f32).round() as usize) * 2 * 2;
            let keep = (frame_bytes * AUDIO_START_LATENCY_FRAMES).next_multiple_of(4);
            if self.pending_audio.len() > keep {
                let drop = self.pending_audio.len() - keep;
                self.pending_audio.drain(..drop);
            }
            let pending = std::mem::take(&mut self.pending_audio);
            let _ = std::thread::Builder::new()
                .name("record-audio".into())
                .spawn(move || {
                    let Ok(mut f) = std::fs::OpenOptions::new().write(true).open(&fifo) else {
                        return;
                    };
                    if !pending.is_empty() && f.write_all(&pending).is_err() {
                        return;
                    }
                    while let Ok(chunk) = arx.recv() {
                        if f.write_all(&chunk).is_err() {
                            break;
                        }
                    }
                });
            audio_tx = Some(atx);
        }

        info!(
            "record: encoding {w}x{h}@{fps} ({}) -> {}",
            if fifo_path.is_some() {
                "a/v"
            } else {
                "video only"
            },
            self.output.display()
        );
        self.pipeline = Some(Pipeline {
            child,
            video_tx,
            audio_tx,
            fifo_path,
        });
    }

    /// Stop capturing and let `ffmpeg` finalize the file. Idempotent: safe to
    /// call from both the exit system and `Drop`.
    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        match self.pipeline.take() {
            Some(p) => {
                eprintln!("record: finalizing, {} frames captured", self.frames_sent);
                p.finish();
            }
            None => warn!("record: no frames captured, nothing written"),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// A running `ffmpeg` encoder plus the channels feeding its pump threads.
struct Pipeline {
    child: Child,
    video_tx: Sender<Vec<u8>>,
    audio_tx: Option<Sender<Vec<u8>>>,
    fifo_path: Option<PathBuf>,
}

impl Pipeline {
    /// Close both inputs and wait for `ffmpeg` to write out the trailer.
    fn finish(self) {
        let Pipeline {
            mut child,
            video_tx,
            audio_tx,
            fifo_path,
        } = self;
        // Dropping the senders ends the pump threads, which drop their pipe
        // handles and signal EOF to ffmpeg; `wait` then returns once it has
        // flushed and finalized the file.
        drop(video_tx);
        drop(audio_tx);
        let _ = child.wait();
        if let Some(fifo) = fifo_path {
            let _ = std::fs::remove_file(fifo);
        }
    }
}

/// Once the window has a real size, build the capture target: a window-sized
/// image that every existing camera renders into, displayed back onto the
/// window, with a [`Readback`] streaming its pixels to [`on_readback`].
fn setup_capture(
    mut commands: Commands,
    mut recorder: ResMut<Recorder>,
    mut images: ResMut<Assets<Image>>,
    window: Query<&Window, With<PrimaryWindow>>,
    emus: Query<&Emulator>,
    mut targets: Query<&mut RenderTarget, With<Camera>>,
) {
    if recorder.capture_ready || recorder.finished {
        return;
    }
    let Ok(window) = window.single() else {
        return;
    };
    let (win_w, win_h) = (window.physical_width(), window.physical_height());
    if win_w == 0 || win_h == 0 {
        return;
    }

    // Size the capture target to an integer multiple of the emulator's live
    // source resolution rather than the window size. The CRT (Lottes) shader
    // rasterizes its scanline/shadow-mask pattern at the render-target
    // resolution; when that has no whole-number relationship to the core's
    // output (e.g. a 574-row source rendered into a 600px window), each source
    // line spreads over a fractional count of output pixels and the pattern
    // beats against the pixel grid — the moiré/interference. Reproducing every
    // source line over an exact whole number of output rows keeps it uniform.
    // We pick the smallest multiple that's still at least the window size, so
    // the recording is never smaller than before. Falls back to the window
    // size if no emulator has reported a size yet.
    let (src_w, src_h) = emus
        .iter()
        .map(|e| (e.width.max(1), e.height.max(1)))
        .next()
        .unwrap_or((win_w, win_h));
    let scale = win_w.div_ceil(src_w).max(win_h.div_ceil(src_h)).max(1);
    let (w, h) = (src_w * scale, src_h * scale);

    // Render target. Must use the default 2D format so it matches the
    // post-process pipeline's color target. COPY_SRC lets the readback copy it.
    let format = TextureFormat::bevy_default();
    recorder.pixel_format = match format {
        TextureFormat::Rgba8UnormSrgb | TextureFormat::Rgba8Unorm => "rgba",
        TextureFormat::Bgra8UnormSrgb | TextureFormat::Bgra8Unorm => "bgra",
        other => {
            warn!("record: unexpected capture format {other:?}; assuming rgba");
            "rgba"
        }
    };
    let size = Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let mut image = Image::new_fill(
        size,
        TextureDimension::D2,
        &[0, 0, 0, 255],
        format,
        RenderAssetUsages::all(),
    );
    image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
        | TextureUsages::COPY_DST
        | TextureUsages::COPY_SRC
        | TextureUsages::RENDER_ATTACHMENT;
    let handle = images.add(image);

    // Redirect every existing camera (emulator + HUD) onto the image.
    for mut target in &mut targets {
        *target = RenderTarget::from(handle.clone());
    }

    // Display the captured image back onto the window so the user still sees
    // it. Its own render layer keeps it isolated from the captured cameras.
    commands.spawn((
        Camera2d,
        Camera {
            order: 1000,
            ..default()
        },
        RenderLayers::layer(DISPLAY_LAYER),
    ));
    commands.spawn((
        Sprite {
            image: handle.clone(),
            custom_size: Some(Vec2::new(window.width(), window.height())),
            ..default()
        },
        RenderLayers::layer(DISPLAY_LAYER),
    ));

    // Continuous per-frame readback of the composited image.
    commands
        .spawn(Readback::texture(handle.clone()))
        .observe(on_readback);

    recorder.width = w;
    recorder.height = h;
    recorder.capture_ready = true;
    eprintln!("record: capture target {w}x{h} @ {} fps", recorder.fps);
}

/// Observer for each completed readback: hand the pixels to the recorder.
fn on_readback(readback: On<ReadbackComplete>, recorder: Option<ResMut<Recorder>>) {
    let Some(mut recorder) = recorder else {
        return;
    };
    if recorder.finished {
        return;
    }
    recorder.push_video(readback.event().data.clone());
}

/// Drain any audio the emulator pushed this frame into the encoder.
fn drain_audio(mut recorder: ResMut<Recorder>) {
    let chunks: Vec<AudioChunk> = recorder.audio_in_rx.get_mut().unwrap().try_iter().collect();
    for (rate, samples) in chunks {
        recorder.write_audio(rate, &samples);
    }
}

/// Finalize as soon as the app is asked to exit, while the world is still alive
/// (`Drop` is the fallback if this doesn't get a chance to run).
fn finalize_on_exit(mut exit: MessageReader<AppExit>, mut recorder: ResMut<Recorder>) {
    if exit.read().next().is_some() {
        recorder.finish();
    }
}

pub struct RecordPlugin;

impl Plugin for RecordPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (setup_capture, drain_audio, finalize_on_exit).run_if(resource_exists::<Recorder>),
        );
    }
}
