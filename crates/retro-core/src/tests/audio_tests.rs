use super::*;
use cpal::SupportedBufferSize;

fn range(min: SampleRate, max: SampleRate, fmt: SampleFormat) -> SupportedStreamConfigRange {
    SupportedStreamConfigRange::new(
        2,
        min,
        max,
        SupportedBufferSize::Range { min: 0, max: 4096 },
        fmt,
    )
}

/// cpal 0.17's WASAPI backend advertises every common rate as its own
/// single-rate range, lowest first. We must land on 48 kHz, not on the
/// 5512 Hz range that happens to come first.
#[test]
fn picks_target_rate_from_wasapi_style_single_rate_ranges() {
    let configs = [5512, 8000, 11025, 22050, 44100, 48000, 96000, 192000]
        .into_iter()
        .flat_map(|r| {
            [
                range(r, r, SampleFormat::I16),
                range(r, r, SampleFormat::F32),
            ]
        });

    let (_, rate) = pick_output_config(configs, 48000).unwrap();
    assert_eq!(rate, 48000);
}

/// A device that can't do the target rate gets the closest it offers.
#[test]
fn falls_back_to_the_nearest_offered_rate() {
    let configs = [22050, 44100]
        .into_iter()
        .map(|r| range(r, r, SampleFormat::F32));

    let (_, rate) = pick_output_config(configs, 48000).unwrap();
    assert_eq!(rate, 44100);
}

/// ALSA/CoreAudio-style wide ranges still resolve to the target rate.
#[test]
fn picks_target_rate_from_a_wide_range() {
    let configs = [range(8000, 192000, SampleFormat::F32)].into_iter();

    let (_, rate) = pick_output_config(configs, 48000).unwrap();
    assert_eq!(rate, 48000);
}

/// A muted sink plays nothing, however much the ring had for it.
#[test]
fn silences_a_full_buffer_at_zero_volume() {
    let mut output = [1.0f32; 8];
    let len = output.len();

    finish_buffer(&mut output, len, 0.0);
    assert_eq!(output, [0.0; 8]);
}

/// An underrun clears the samples it could not fill, rather than leaving the
/// previous callback's -- which the volume above never touched.
#[test]
fn clears_what_an_underrun_left_behind() {
    let mut output = [1.0f32; 8];

    finish_buffer(&mut output, 4, 0.5);
    assert_eq!(output, [0.5, 0.5, 0.5, 0.5, 0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn audio_out_pushes_into_the_sinks_ring() {
    let sink = AudioSink::default();
    {
        let mut state = sink.state();
        state.sample_rate = 44100.0;
        state.resampler = Some(AudioResampler::new(44100, 44100).unwrap());
    }
    let (producer, _consumer) = ringbuf::HeapRb::<f32>::new(RING_SIZE).split();
    sink.set_producer(producer);

    let out = sink.out();
    std::thread::spawn(move || out.push(44100.0, &[0.5; 4096]))
        .join()
        .unwrap();

    assert!(sink.occupied_len() > 0);
}

#[test]
fn the_rate_adjust_drains_a_full_sink_and_fills_an_empty_one() {
    let (min, max) = (3000, 18000);
    let target = min + (max - min) / 2;

    assert!((rate_adjust(target as f64, min, max) - 1.0).abs() < 1e-9);
    assert!((rate_adjust(max as f64, min, max) - 0.99).abs() < 1e-9);
    assert!((rate_adjust(min as f64, min, max) - 1.01).abs() < 1e-9);
    assert_eq!(rate_adjust(1e9, min, max), 0.99);
    assert!(rate_adjust(0.0, min, max) <= 1.01);
}

#[test]
fn the_fill_starts_at_the_first_reading_and_then_follows_slowly() {
    assert_eq!(smooth_fill(0.0, 4000), 4000.0);
    assert_eq!(smooth_fill(4000.0, 8000), 4200.0);
}
