use std::sync::Mutex;

use anyhow::Result;

use bevy::prelude::*;

use cpal::{
    SampleFormat, SampleRate, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};

use ringbuf::{
    HeapCons, HeapProd,
    traits::{Observer, *},
};

use rubato::{FastFixedIn, PolynomialDegree, Resampler};

/// Largest fractional drift correction [`AudioResampler::set_adjust`] will
/// honour. The async resampler is built with this much relative-ratio headroom
/// so corrections can be applied on the fly without rebuilding. Kept well above
/// the controller's own clamp so it never saturates here.
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

    /// Nudge the output/input ratio by `adjust` (a small signed fraction, e.g.
    /// `+0.002` for +0.2%) to compensate for clock drift between the emulator
    /// and the audio device, without rebuilding the resampler.
    ///
    /// A positive `adjust` raises the effective input rate, so the resampler
    /// emits *fewer* output frames per input frame and the downstream ring
    /// buffer drains; a negative `adjust` does the reverse and lets it fill.
    /// The change is ramped over the next chunk to avoid zipper noise, and
    /// clamped to the headroom the resampler was built with.
    pub fn set_adjust(&mut self, adjust: f64) {
        let adjust = adjust.clamp(-MAX_RATIO_ADJUST, MAX_RATIO_ADJUST);
        // rel < 1 when adjust > 0: faster input -> fewer output frames.
        let rel = 1.0 / (1.0 + adjust);
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

pub fn init_audio_stream(mut consumer: HeapCons<f32>) -> Result<(f32, cpal::Stream)> {
    let host = cpal::default_host();
    let device = host.default_output_device().unwrap();

    let target = SampleRate(48000);

    let supported = device
        .supported_output_configs()?
        .find(|c| {
            c.channels() == 2
                && c.sample_format() == SampleFormat::F32
                && c.min_sample_rate() <= target
        })
        .expect("no supported config");
    let sample_rate = target.min(supported.max_sample_rate());

    // We continuously adjust the resample ratio based on how full the ring
    // buffer is, so a small buffer is desirable for tight feedback. Prefer 2048
    // frames, but clamp into the device's advertised range: if the smallest
    // supported buffer is larger than 2048 we take that (the lowest supported),
    // and if the range is unknown we let the backend choose. cpal's CoreAudio
    // backend rejects out-of-range fixed sizes with `StreamConfigNotSupported`,
    // so this clamp is what keeps macOS happy.
    const PREFERRED_BUFFER: u32 = 2048;
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
        config.sample_rate.0, config.channels, config.buffer_size
    );

    let stream = device.build_output_stream(
        &config,
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let count = consumer.pop_slice(output);
            if count == 0 {
                output.fill(0.0);
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok((config.sample_rate.0 as f32, stream))
}

#[derive(Default)]
pub struct AudioSink {
    pub producer: Option<Mutex<HeapProd<f32>>>,
    pub sample_rate: f32,
    pub stream: Option<SendStream>,
    pub resampler: Option<AudioResampler>,
}

impl AudioSink {
    pub fn activate(&mut self) {
        let (producer, consumer) = ringbuf::HeapRb::<f32>::new(4096 * 8).split();
        let Ok((sample_rate, stream)) = init_audio_stream(consumer) else {
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

    pub fn push_audio(&mut self, from: f32, samples: &[i16]) {
        if let Some(resampler) = &mut self.resampler {
            let res = resampler.process(from as u32, samples, |l, r| {
                if let Some(producer) = &self.producer {
                    producer.lock().unwrap().push_iter([l, r].into_iter());
                }
            });
            if let Err(e) = res {
                warn!("audio resample error: {e}");
            }
        }
    }

    pub(crate) fn set_adjust(&mut self, audio_rate_adjust: f64) {
        if let Some(resampler) = &mut self.resampler {
            resampler.set_adjust(audio_rate_adjust);
        }
    }

    pub(crate) fn occupied_len(&self) -> usize {
        if let Some(producer) = &self.producer {
            producer.lock().unwrap().occupied_len()
        } else {
            6000
        }
    }
}
