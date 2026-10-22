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
    rec.push_i16(1000, &[0, 0, 1, 1]);
    assert_eq!(rec.samples, [0, 0, 1, 1]);
    assert_eq!(rec.elapsed(), Duration::from_millis(2));
}

#[test]
fn start_level_keeps_from_first_non_zero_sample() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), None));
    rec.push_i16(1000, &[0, 0, 0, 0, 10, 0, 20, 0]);
    assert_eq!(rec.state, State::Waiting);
    assert_eq!(rec.elapsed(), Duration::ZERO);
    rec.push_i16(1000, &[30000, 0]);
    assert_eq!(rec.state, State::Recording);
    assert_eq!(rec.samples, [10, 0, 20, 0, 30000, 0]);
}

#[test]
fn zero_gap_restarts_pre_roll() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), None));
    rec.push_i16(44100, &[10, 10]);
    rec.push_i16(44100, &[0; 2 * 1000]);
    rec.push_i16(44100, &[5, 5, 30000, 0]);
    assert_eq!(rec.samples, [5, 5, 30000, 0]);
}

#[test]
fn stop_on_silence_trims_the_tail() {
    let mut rec = AudioRecorder::new("x.wav", opts(Some(0.5), Some(Duration::from_millis(3))));
    rec.push_i16(1000, &[30000, 0, 1, 1, 1, 1]);
    assert!(!rec.is_done());
    rec.push_i16(1000, &[1, 1, 30000, 30000]);
    assert!(rec.is_done());
    assert_eq!(rec.samples, [30000, 0]);
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
    rec.push_i16(1000, &[1, 1]);
    rec.push_f32(48000, 1.0, -2.0);
    assert_eq!(rec.samples, [32767, -32767]);
    assert_eq!(rec.rate, 48000);
}

#[test]
fn finish_writes_a_wav() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rec.wav");
    let mut rec = AudioRecorder::new(&path, opts(None, None));
    rec.push_i16(22050, &[1, 2, 3, 4]);
    rec.finish().unwrap().join().unwrap();
    let reader = hound::WavReader::open(&path).unwrap();
    assert_eq!(reader.spec().sample_rate, 22050);
    assert_eq!(reader.into_samples::<i16>().map(Result::unwrap).collect::<Vec<_>>(), [1, 2, 3, 4]);
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
    let tone: Vec<i16> = (0..44100).map(|i| ((i as f32 * 0.05).sin() * 8000.0) as i16).collect();
    rec.push_i16(44100, &tone);
    rec.finish().unwrap().join().unwrap();
    let mp3 = std::fs::read(path.with_extension("mp3")).unwrap();
    assert!(mp3.len() > 1000);
}
