use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Result;

use tracing::{debug, error, info, warn};

use cpal::{
    SampleFormat, SampleRate, StreamConfig, SupportedStreamConfigRange,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

use ringbuf::{
    HeapCons, HeapProd,
    traits::{Observer, *},
};

use rubato::{FastFixedIn, PolynomialDegree, Resampler};

use crate::audio_recorder::{AudioRecorder, RecorderOptions};

/// How far from `1.0` [`AudioResampler::set_adjust`] may move the ratio; the
/// resampler is built with this much headroom.
const MAX_RATIO_ADJUST: f64 = 0.05;

/// Resamples interleaved stereo audio from the core's native rate to the
/// output device rate, converting the core's `i16` samples to `f32` along the
/// way.
///
/// The core hands us a variable number of frames each call, while the
/// underlying [`FastFixedIn`] resampler wants a fixed input chunk, so incoming
/// samples are deinterleaved into per-channel buffers and consumed one full
/// chunk at a time.
///
/// [`FastFixedIn`] is an *asynchronous* resampler whose ratio can be nudged via
/// [`set_resample_ratio_relative`](Resampler::set_resample_ratio_relative)
/// without rebuilding. That is what makes per-frame drift correction cheap:
/// only a genuine change of the core's *nominal* sample rate triggers a rebuild.
pub struct AudioResampler {
    inner: FastFixedIn<f32>,
    /// Deinterleaved input awaiting a full chunk, one buffer per channel.
    in_buf: [Vec<f32>; 2],
    /// Scratch output buffers, one per channel.
    out: [Vec<f32>; 2],
    chunk_size: usize,
    /// Current nominal input (core) sample rate, tracked so [`process`] can skip
    /// rebuilding when the rate is unchanged.
    from: u32,
    /// Output (device) sample rate, needed to rebuild `inner` on a rate change.
    to: u32,
}

impl AudioResampler {
    pub fn new(from: u32, to: u32) -> Result<Self> {
        let chunk_size = 1024;
        let inner = FastFixedIn::<f32>::new(
            to as f64 / from as f64,
            1.0 + MAX_RATIO_ADJUST,
            PolynomialDegree::Cubic,
            chunk_size,
            2,
        )?;
        let out_max = inner.output_frames_max();
        Ok(Self {
            inner,
            in_buf: [Vec::new(), Vec::new()],
            out: [vec![0.0; out_max], vec![0.0; out_max]],
            chunk_size,
            from,
            to,
        })
    }

    /// Multiply the output/input ratio by `adjust`: `1.0` leaves it alone,
    /// `0.99` emits 1% fewer output frames (the ring drains), `1.01` 1% more.
    pub fn set_adjust(&mut self, adjust: f64) {
        let rel = adjust.clamp(1.0 - MAX_RATIO_ADJUST, 1.0 + MAX_RATIO_ADJUST);
        if let Err(e) = self.inner.set_resample_ratio_relative(rel, true) {
            warn!("audio ratio adjust failed: {e}");
        }
    }

    /// Feeds interleaved stereo `i16` samples captured at `from` Hz, invoking
    /// `sink` with each resampled `(left, right)` `f32` frame.
    ///
    /// If `from` differs from the rate the resampler was last built for, the
    /// resampler is rebuilt for the new ratio. Before that, whatever the old
    /// resampler still holds — the trailing partial chunk plus its internal
    /// delay — is flushed through `sink`, so the rate change neither drops nor
    /// mis-pitches already-captured audio. Calls keeping the same `from` skip
    /// the rebuild, so this is cheap to invoke every frame.
    pub fn process(
        &mut self,
        from: u32,
        samples: &[i16],
        mut sink: impl FnMut(f32, f32),
    ) -> Result<()> {
        // `from == 0` means the core hasn't reported a rate yet; keep the
        // current resampler rather than rebuilding with a bogus ratio.
        if from != 0 && from != self.from {
            // `process` always drains down to a sub-chunk remainder, so the
            // buffer holds fewer than `chunk_size` frames here. Zero-pad that
            // remainder to a full chunk and push it through the old resampler:
            // the captured frames (and the previous chunk's delayed tail) come
            // out, while the padding zeros land in the discarded next block.
            let remainder = self.in_buf[0].len();
            if remainder > 0 {
                self.in_buf[0].resize(self.chunk_size, 0.0);
                self.in_buf[1].resize(self.chunk_size, 0.0);
                let [o0, o1] = &mut self.out;
                let (_, written) = self.inner.process_into_buffer(
                    &[&self.in_buf[0][..], &self.in_buf[1][..]],
                    &mut [&mut o0[..], &mut o1[..]],
                    None,
                )?;
                for i in 0..written {
                    sink(o0[i], o1[i]);
                }
            }
            self.in_buf[0].clear();
            self.in_buf[1].clear();

            // Rebuild for the new ratio and resize the scratch output buffers.
            self.inner = FastFixedIn::<f32>::new(
                self.to as f64 / from as f64,
                1.0 + MAX_RATIO_ADJUST,
                PolynomialDegree::Cubic,
                self.chunk_size,
                2,
            )?;
            let out_max = self.inner.output_frames_max();
            self.out = [vec![0.0; out_max], vec![0.0; out_max]];
            self.from = from;
        }

        for frame in samples.chunks_exact(2) {
            self.in_buf[0].push(frame[0] as f32 / 32767.0);
            self.in_buf[1].push(frame[1] as f32 / 32767.0);
        }

        let mut consumed = 0;
        while self.in_buf[0].len() - consumed >= self.chunk_size {
            let range = consumed..consumed + self.chunk_size;
            let [o0, o1] = &mut self.out;
            let (_, written) = self.inner.process_into_buffer(
                &[&self.in_buf[0][range.clone()], &self.in_buf[1][range]],
                &mut [&mut o0[..], &mut o1[..]],
                None,
            )?;
            for i in 0..written {
                sink(o0[i], o1[i]);
            }
            consumed += self.chunk_size;
        }

        if consumed > 0 {
            self.in_buf[0].drain(..consumed);
            self.in_buf[1].drain(..consumed);
        }
        Ok(())
    }
}

/// Wrapper that makes [`cpal::Stream`] `Send + Sync`.
///
/// cpal marks `Stream` as `!Send + !Sync` (`NotSendSyncAcrossAllPlatforms`)
/// because a few backends require the handle to stay on its creating thread.
/// We never call any functions on it so should be fine.
pub struct SendStream(#[allow(dead_code)] cpal::Stream);

// SAFETY: the ALSA stream handle is safe to move and drop across threads, and
// it is never accessed after `init_audio_stream` returns it.
unsafe impl Send for SendStream {}
unsafe impl Sync for SendStream {}

/// Picks the stereo `f32` output config to open, and the rate to open it at,
/// from the ranges a device advertises. Returns the range closest to `target`,
/// preferring one that covers it outright.
///
/// Every range has to be clamped against, not just filtered with
/// `min_sample_rate() <= target`: since cpal 0.17 the WASAPI backend no longer
/// asks `IsFormatSupported` and instead advertises each entry of cpal's
/// `COMMON_SAMPLE_RATES` as its own single-rate range, letting the audio engine
/// convert via `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`. The first of those ranges
/// is 5512 Hz, so picking the first range with `min <= target` and then taking
/// `target.min(max)` opened the output stream at 5512 Hz on Windows.
fn pick_output_config(
    configs: impl Iterator<Item = SupportedStreamConfigRange>,
    target: SampleRate,
) -> Option<(SupportedStreamConfigRange, SampleRate)> {
    let supported = configs
        .filter(|c| c.channels() == 2 && c.sample_format() == SampleFormat::F32)
        .min_by_key(|c| target.abs_diff(target.clamp(c.min_sample_rate(), c.max_sample_rate())))?;
    let rate = target.clamp(supported.min_sample_rate(), supported.max_sample_rate());
    Some((supported, rate))
}

/// Output level of one sink, shared with its audio callback. `f32` bits in an
/// atomic so the cross fade can ramp it from the main thread while the callback
/// reads it, without touching the samples already queued behind it.
#[derive(Clone)]
pub struct Volume(Arc<AtomicU32>);

impl Volume {
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }

    pub fn set(&self, level: f32) {
        self.0
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

impl Default for Volume {
    fn default() -> Self {
        Self(Arc::new(AtomicU32::new(1.0f32.to_bits())))
    }
}

/// Ready one callback buffer: `count` samples came out of the ring, so scale
/// those to `level` and silence whatever is left. The tail matters — cpal hands
/// out the previous callback's buffer, so an underrun that left it alone would
/// replay those samples, at full volume however quiet this sink is meant to be.
fn finish_buffer(output: &mut [f32], count: usize, level: f32) {
    if level < 1.0 {
        for sample in &mut output[..count] {
            *sample *= level;
        }
    }
    output[count..].fill(0.0);
}

pub fn init_audio_stream(
    mut consumer: HeapCons<f32>,
    volume: Volume,
) -> Result<(f32, cpal::Stream)> {
    let host = cpal::default_host();
    let device = host.default_output_device().unwrap();

    // Run at whatever rate the device itself runs at. Nothing we play is
    // sourced above 44.1 kHz, so there is nothing to gain from asking for a
    // higher rate -- and on WASAPI the device rate is the shared-mode mix rate,
    // so matching it keeps Windows' audio engine from resampling on top of us.
    let target: SampleRate = device
        .default_output_config()
        .map(|c| c.sample_rate())
        .unwrap_or(48000);

    let (supported, sample_rate) = pick_output_config(device.supported_output_configs()?, target)
        .expect("no supported config");

    // A small buffer keeps latency down and lets the ring run near empty. Clamp
    // into the device's advertised range: cpal's CoreAudio backend rejects
    // out-of-range fixed sizes with `StreamConfigNotSupported`.
    const PREFERRED_BUFFER: u32 = 512;
    let buffer_size = match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            cpal::BufferSize::Fixed(PREFERRED_BUFFER.clamp(*min, *max))
        }
        cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
    };

    let mut config: StreamConfig = supported.with_sample_rate(sample_rate).into();
    config.channels = 2;
    config.buffer_size = buffer_size;

    info!(
        "cpal cfg: rate={} channels={} buffer={:?}",
        config.sample_rate, config.channels, config.buffer_size
    );

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let count = consumer.pop_slice(output);
            if count > 0 && count < output.len() {
                debug!("Audio drop");
            }
            finish_buffer(output, count, volume.get());
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok((config.sample_rate as f32, stream))
}

/// Samples the ring between an emulator and its audio callback holds.
pub const RING_SIZE: usize = 4096 * 8;

#[derive(Default)]
pub struct AudioSink {
    pub producer: Option<Mutex<HeapProd<f32>>>,
    pub sample_rate: f32,
    pub stream: Option<SendStream>,
    pub resampler: Option<AudioResampler>,
    /// Survives [`AudioSink::deactivate`], so a level set while the sink is
    /// closed is still in force when it opens again.
    volume: Volume,
    recorder: Option<AudioRecorder>,
}

impl AudioSink {
    pub fn activate(&mut self) {
        let (producer, consumer) = ringbuf::HeapRb::<f32>::new(RING_SIZE).split();
        let Ok((sample_rate, stream)) = init_audio_stream(consumer, self.volume.clone()) else {
            error!("Could not init audio");
            return;
        };

        let resampler = AudioResampler::new(44100, sample_rate as u32)
            .expect("Failed to create audio resampler");

        self.stream = Some(SendStream(stream));
        self.sample_rate = sample_rate;
        self.producer = Some(Mutex::new(producer));
        self.resampler = Some(resampler);
    }

    pub fn deactivate(&mut self) {
        self.stream = None;
        self.producer = None;
        self.resampler = None;
    }

    /// Set this sink's output level, `0` (silent) to `1`.
    pub fn set_volume(&self, level: f32) {
        self.volume.set(level);
    }

    pub fn volume(&self) -> f32 {
        self.volume.get()
    }

    pub fn start_recording(&mut self, path: impl Into<std::path::PathBuf>, opts: RecorderOptions) {
        self.stop_recording();
        self.recorder = Some(AudioRecorder::new(path, opts));
    }

    pub fn stop_recording(&mut self) {
        if let Some(recorder) = self.recorder.take() {
            recorder.finish();
        }
    }

    pub fn recording_time(&self) -> Option<std::time::Duration> {
        self.recorder.as_ref().map(AudioRecorder::elapsed)
    }

    pub fn push_audio(&mut self, from: f32, samples: &[i16]) {
        let mut recorder = self.recorder.as_mut();
        if let Some(rec) = &mut recorder {
            rec.push_i16(from as u32, samples);
        }
        if let Some(resampler) = &mut self.resampler
            && let Some(producer) = &self.producer
        {
            let rate = self.sample_rate as u32;
            let mut p = producer.lock().unwrap();
            let res = resampler.process(from as u32, samples, |l, r| {
                p.push_iter([l, r].into_iter());
                if let Some(rec) = &mut recorder {
                    rec.push_f32(rate, l, r);
                }
            });
            if let Err(e) = res {
                warn!("audio resample error: {e}");
            }
        }
        if self.recorder.as_ref().is_some_and(AudioRecorder::is_done) {
            self.stop_recording();
        }
    }

    pub fn set_adjust(&mut self, audio_rate_adjust: f64) {
        if let Some(resampler) = &mut self.resampler {
            resampler.set_adjust(audio_rate_adjust);
        }
    }

    pub fn occupied_len(&self) -> usize {
        if let Some(producer) = &self.producer {
            producer.lock().unwrap().occupied_len()
        } else {
            6000
        }
    }
}

#[cfg(test)]
#[path = "tests/audio_tests.rs"]
mod tests;
