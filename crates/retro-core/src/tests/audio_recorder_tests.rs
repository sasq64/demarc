use super::*;

fn opts(start: Option<f32>, stop: Option<Duration>) -> RecorderOptions {
    RecorderOptions {
        start_on_audio: start,
        stop_on_silence: stop,
        ..Default::default()
    }
}

#[test]
fn records_at_once_without_start_level() {
    let mut rec = AudioRecorder::new("x.wav", opts(None, None));
    rec.push(1000, &[0.0, 0.0, 0.1, 0.1]);
    assert_eq!(rec.samples, [0.0, 0.0, 0.1, 0.1]);
    assert_eq!(rec.elapsed(), Duration::from_millis(2));
}

#[test]
fn start_level_keeps_from_first_non_zero_sample() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), None));
    rec.push(1000, &[0.0, 0.0, 0.0, 0.0, 0.1, 0.0, 0.2, 0.0]);
    assert_eq!(rec.state, State::Waiting);
    assert_eq!(rec.elapsed(), Duration::ZERO);
    rec.push(1000, &[0.9, 0.0]);
    assert_eq!(rec.state, State::Recording);
    assert_eq!(rec.samples, [0.1, 0.0, 0.2, 0.0, 0.9, 0.0]);
}

#[test]
fn zero_gap_restarts_pre_roll() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), None));
    rec.push(44100, &[0.1, 0.1]);
    rec.push(44100, &[0.0; 2 * 1000]);
    rec.push(44100, &[0.05, 0.05, 0.9, 0.0]);
    assert_eq!(rec.samples, [0.05, 0.05, 0.9, 0.0]);
}

#[test]
fn stop_on_silence_trims_the_tail() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), Some(Duration::from_millis(3))));
    rec.push(1000, &[0.9, 0.0, 0.01, 0.01, 0.01, 0.01]);
    assert!(!rec.is_done());
    rec.push(1000, &[0.01, 0.01, 0.9, 0.9]);
    assert!(rec.is_done());
    assert_eq!(rec.samples, [0.9, 0.0]);
}

#[test]
fn source_selects_what_is_kept() {
    let mut rec = AudioRecorder::new(
        "x.wav",
        RecorderOptions {
            source: RecordSource::Resampled,
            ..Default::default()
        },
    );
    rec.push(1000, &[0.1, 0.1]);
    rec.push_resampled(48000, 1.0, -2.0);
    assert_eq!(rec.samples, [1.0, -2.0]);
    assert_eq!(rec.rate, 48000);
}

#[test]
fn finish_writes_a_wav() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rec.wav");
    let mut rec = AudioRecorder::new(&path, opts(None, None));
    rec.push(22050, &[0.25, -0.5, 1.5, 0.0]);
    rec.finish().unwrap().join().unwrap();
    let reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(reader.spec().sample_rate, 22050);
    assert_eq!(
        reader.into_samples::<f32>().map(Result::unwrap).collect::<Vec<_>>(),
        [0.25, -0.5, 1.5, 0.0]
    );
}

#[cfg(feature = "mp3")]
#[test]
fn finish_also_writes_an_mp3() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rec.wav");
    let mut rec = AudioRecorder::new(
        &path,
        RecorderOptions {
            mp3: true,
            ..Default::default()
        },
    );
    let tone: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.25).collect();
    rec.push(44100, &tone);
    rec.finish().unwrap().join().unwrap();
    let mp3 = std::fs::read(path.with_extension("mp3")).unwrap();
    assert!(mp3.len() > 1000);
}
