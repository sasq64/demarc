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
