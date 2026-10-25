use std::path::{Path, PathBuf};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use tracing::{error, info, warn};

use crate::audio::SILENCE_LEVEL;


#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordSource {
    /// The core's own samples, at the core's rate.
    #[default]
    Original,
    /// What goes to the device, after resampling and drift correction.
    Resampled,
}

#[derive(Debug, Clone, Default)]
pub struct RecorderOptions {
    pub source: RecordSource,
    /// Level (0..1) the audio has to reach before anything is kept.
    pub start_on_audio: Option<f32>,
    /// Stop once the audio has stayed below the level this long.
    pub stop_on_silence: Option<Duration>,
    pub mp3: bool,
    pub tags: Tags,
}

/// ID3 tags for the MP3.
#[derive(Debug, Clone, Default)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<i32>,
    /// JPEG front cover.
    pub cover: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Waiting,
    Recording,
    Done,
}

pub struct AudioRecorder {
    opts: RecorderOptions,
    path: PathBuf,
    rate: u32,
    /// Interleaved stereo, kept past ±1 when the source goes there.
    samples: Vec<f32>,
    state: State,
    /// Consecutive frames below the level: zero frames while waiting, quiet
    /// frames while recording.
    quiet_frames: usize,
}

impl AudioRecorder {
    pub fn new(path: impl Into<PathBuf>, opts: RecorderOptions) -> Self {
        let state = if opts.start_on_audio.is_some() {
            State::Waiting
        } else {
            State::Recording
        };
        Self {
            opts,
            path: path.into(),
            rate: 0,
            samples: Vec::new(),
            state,
            quiet_frames: 0,
        }
    }

    pub fn push(&mut self, rate: u32, samples: &[f32]) {
        if self.opts.source != RecordSource::Original {
            return;
        }
        self.set_rate(rate);
        for frame in samples.chunks_exact(2) {
            self.push_frame(frame[0], frame[1]);
        }
    }

    pub fn push_resampled(&mut self, rate: u32, l: f32, r: f32) {
        if self.opts.source != RecordSource::Resampled {
            return;
        }
        self.set_rate(rate);
        self.push_frame(l, r);
    }

    fn set_rate(&mut self, rate: u32) {
        if rate == 0 || rate == self.rate {
            return;
        }
        if self.rate == 0 {
            self.rate = rate;
        } else {
            warn!("recording: rate changed {} -> {rate}, keeping {}", self.rate, self.rate);
        }
    }

    fn push_frame(&mut self, l: f32, r: f32) {
        let peak = l.abs().max(r.abs());
        match self.state {
            State::Waiting => {
                if peak == 0.0 {
                    self.quiet_frames += 1;
                    // A gap of digital silence restarts the pre-roll.
                    if self.quiet_frames > self.rate.max(44100) as usize / 100 {
                        self.samples.clear();
                    }
                    if self.samples.is_empty() {
                        return;
                    }
                } else {
                    self.quiet_frames = 0;
                }
                self.samples.extend([l, r]);
                if peak > self.opts.start_on_audio.unwrap_or(SILENCE_LEVEL) {
                    self.state = State::Recording;
                }
            }
            State::Recording => {
                self.samples.extend([l, r]);
                if peak > SILENCE_LEVEL {
                    self.quiet_frames = 0;
                    return;
                }
                self.quiet_frames += 1;
                if let Some(silence) = self.opts.stop_on_silence
                    && self.quiet_frames as f64 >= silence.as_secs_f64() * self.rate() as f64
                {
                    let keep = self.samples.len() - self.quiet_frames * 2;
                    self.samples.truncate(keep);
                    self.state = State::Done;
                }
            }
            State::Done => {}
        }
    }

    fn rate(&self) -> u32 {
        if self.rate == 0 { 44100 } else { self.rate }
    }

    pub fn elapsed(&self) -> Duration {
        if self.state == State::Waiting {
            return Duration::ZERO;
        }
        Duration::from_secs_f64((self.samples.len() / 2) as f64 / self.rate() as f64)
    }

    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    pub fn set_cover(&mut self, jpeg: Vec<u8>) {
        self.opts.tags.cover = Some(jpeg);
    }

    /// Recording, and set to stop by itself when the audio goes quiet.
    pub fn stops_on_silence(&self) -> bool {
        self.state == State::Recording && self.opts.stop_on_silence.is_some()
    }

    /// Writes the recording in a thread of its own.
    pub fn finish(self) -> Option<JoinHandle<()>> {
        if self.state == State::Waiting || self.samples.is_empty() {
            info!("recording: nothing recorded");
            return None;
        }
        Some(std::thread::spawn(move || {
            if let Err(e) = write_wav(&self.path, self.rate(), &self.samples) {
                error!("recording: writing {}: {e}", self.path.display());
                return;
            }
            info!("recording: saved {}", self.path.display());
            if self.opts.mp3 {
                let mp3 = self.path.with_extension("mp3");
                match write_mp3(&mp3, self.rate(), &self.samples, &self.opts.tags) {
                    Ok(()) => info!("recording: saved {}", mp3.display()),
                    Err(e) => error!("recording: writing {}: {e}", mp3.display()),
                }
            }
        }))
    }
}

fn write_wav(path: &Path, rate: u32, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

#[cfg(feature = "mp3")]
fn write_mp3(path: &Path, rate: u32, samples: &[f32], tags: &Tags) -> Result<()> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, InterleavedPcm, Quality};

    let err = |e: mp3lame_encoder::BuildError| anyhow::anyhow!("{e}");
    let mut builder = Builder::new().ok_or_else(|| anyhow::anyhow!("LAME init failed"))?;
    builder.set_num_channels(2).map_err(err)?;
    builder.set_sample_rate(rate).map_err(err)?;
    builder.set_brate(Bitrate::Kbps192).map_err(err)?;
    builder.set_quality(Quality::Best).map_err(err)?;
    let mut encoder = builder.build().map_err(err)?;

    let mut out = Vec::new();
    out.reserve(mp3lame_encoder::max_required_buffer_size(samples.len() / 2));
    encoder
        .encode_to_vec(InterleavedPcm(samples), &mut out)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    out.reserve(7200);
    encoder
        .flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    std::fs::write(path, out)?;
    write_tags(path, tags)
}

#[cfg(feature = "mp3")]
fn write_tags(path: &Path, tags: &Tags) -> Result<()> {
    use id3::TagLike;

    let mut tag = id3::Tag::new();
    if let Some(title) = &tags.title {
        tag.set_title(title);
    }
    if let Some(artist) = &tags.artist {
        tag.set_artist(artist);
    }
    if let Some(album) = &tags.album {
        tag.set_album(album);
    }
    if let Some(year) = tags.year {
        tag.set_year(year);
    }
    if let Some(cover) = &tags.cover {
        tag.add_frame(id3::frame::Picture {
            mime_type: "image/jpeg".into(),
            picture_type: id3::frame::PictureType::CoverFront,
            description: String::new(),
            data: cover.clone(),
        });
    }
    if tag.frames().next().is_some() {
        tag.write_to_path(path, id3::Version::Id3v24)?;
    }
    Ok(())
}

#[cfg(not(feature = "mp3"))]
fn write_mp3(_path: &Path, _rate: u32, _samples: &[f32], _tags: &Tags) -> Result<()> {
    anyhow::bail!("built without mp3 support")
}

#[cfg(test)]
#[path = "tests/audio_recorder_tests.rs"]
mod tests;
