use super::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// A player standing in for the awkward end of `musix`: it answers the
/// first `empty_reads` requests with nothing (as UADE does while it boots
/// the Amiga replayer), then produces a constant tone, and records the size
/// of every request it was given.
struct FakePlayer {
    empty_reads: u32,
    reads: u32,
    /// Never produce anything, whatever is asked.
    always_empty: bool,
    sizes: Arc<Mutex<Vec<usize>>>,
    files: Vec<PathBuf>,
}

impl MusixPlayer for FakePlayer {
    fn get_song_files(&self) -> &Vec<PathBuf> {
        &self.files
    }

    fn get_frequency(&self) -> u32 {
        44100
    }

    fn get_samples(&mut self, target: &mut [i16]) -> usize {
        self.sizes.lock().unwrap().push(target.len());
        self.reads += 1;
        if self.always_empty || self.reads <= self.empty_reads {
            return 0;
        }
        target.fill(1000);
        target.len()
    }
}

/// The shipped visualization script. `from_player` attaches none, and
/// `MusicEmu::new` only takes one if it is handed the path, so the tests
/// that care about pixels ask for it explicitly.
fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("system/lua/scope.lua")
}

/// The colour `scope.lua` draws the left channel in. Written out through
/// Lua's `rgb()` and read back as native bytes, it must land on exactly
/// what the Rust `rgb` here produces — which makes every assertion on it
/// a check of the byte order too.
const LEFT_TRACE: u32 = rgb(0x50, 0xe0, 0xa0);

fn with_script(mut emu: MusicEmu) -> MusicEmu {
    emu.vis = Some(Visualizer::new(&script(), WIDTH, HEIGHT).expect("shipped scope.lua"));
    emu
}

fn fake(empty_reads: u32, always_empty: bool) -> (MusicEmu, Arc<Mutex<Vec<usize>>>) {
    fake_with_channels(empty_reads, always_empty, 2)
}

fn fake_with_channels(
    empty_reads: u32,
    always_empty: bool,
    channels: u32,
) -> (MusicEmu, Arc<Mutex<Vec<usize>>>) {
    let sizes = Arc::new(Mutex::new(Vec::new()));
    let player = FakePlayer {
        empty_reads,
        reads: 0,
        always_empty,
        sizes: sizes.clone(),
        files: Vec::new(),
    };
    (
        MusicEmu::from_player(Box::new(player), "".into(), 44100.0, channels),
        sizes,
    )
}

/// Metadata reaches the script in the encoding it is drawn in: one byte per
/// character, Latin-1, with everything the font has no glyph for turned
/// into a `?` rather than into a pair of mojibake bytes.
#[test]
fn meta_is_re_encoded_as_latin1() {
    // Straight through.
    assert_eq!(to_latin1("Commando"), b"Commando");
    // In Latin-1, so one byte each rather than the two UTF-8 spends.
    assert_eq!(to_latin1("Björn Ålder"), b"Bj\xf6rn \xc5lder");
    // Outside it, so replaced: a curly quote, a long dash, a kana.
    assert_eq!(to_latin1("Rob\u{2019}s \u{2014} \u{30c6}"), b"Rob?s ? ?");
    // Control codes have no glyph worth drawing either.
    assert_eq!(to_latin1("a\tb\n"), b"a?b?");
}

/// A mono player must still fill a stereo frame: each sample is doubled, so
/// a second of mono is a second of stereo rather than half of one.
#[test]
fn mono_output_is_doubled_into_stereo() {
    // A ramp, so a duplicated pair is distinguishable from two neighbouring
    // samples that happen to be equal.
    struct MonoRamp {
        next: i16,
        files: Vec<PathBuf>,
    }
    impl MusixPlayer for MonoRamp {
        fn get_song_files(&self) -> &Vec<PathBuf> {
            &self.files
        }
        fn get_frequency(&self) -> u32 {
            44100
        }
        fn get_samples(&mut self, target: &mut [i16]) -> usize {
            for slot in target.iter_mut() {
                *slot = self.next;
                self.next = self.next.wrapping_add(1);
            }
            target.len()
        }
    }

    let player = MonoRamp {
        next: 0,
        files: Vec::new(),
    };
    let mut emu = MusicEmu::from_player(Box::new(player), "".into(), 44100.0, 1);
    assert!(emu.run());

    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert_eq!(samples.len(), 44100 / FRAME_RATE as usize * 2);
    for (i, pair) in samples.chunks(2).enumerate() {
        assert_eq!(pair[0], pair[1], "pair {i} is not duplicated: {pair:?}");
        assert_eq!(pair[0], i as i16, "pair {i} is out of order: {pair:?}");
    }

    // And the doubling holds across the chunk boundary, where the in-place
    // expansion has to leave the unread tail alone.
    let mut later = Vec::new();
    for _ in 0..30 {
        emu.run();
        emu.with_audio(&mut |s| later.extend_from_slice(s));
    }
    assert!(later.len() > CHUNK * 2, "not enough audio to cross a chunk");
    assert!(
        later.chunks(2).all(|pair| pair[0] == pair[1]),
        "a pair was not duplicated after the first chunk"
    );
}

/// The scope draws what the speakers are playing, not what was just
/// rendered: a burst of audio must appear on screen [`SCOPE_DELAY`] later,
/// once it has worked its way through the frontend's audio buffers.
#[test]
fn the_scope_lags_the_audio_by_the_output_delay() {
    /// One frame of full-scale tone, then silence for as long as it is asked.
    struct OneBurst {
        left: usize,
        files: Vec<PathBuf>,
    }
    impl MusixPlayer for OneBurst {
        fn get_song_files(&self) -> &Vec<PathBuf> {
            &self.files
        }
        fn get_frequency(&self) -> u32 {
            44100
        }
        fn get_samples(&mut self, target: &mut [i16]) -> usize {
            let burst = self.left.min(target.len());
            target[..burst].fill(30000);
            target[burst..].fill(0);
            self.left -= burst;
            target.len()
        }
    }

    let frame = (44100.0 / FRAME_RATE) as usize * 2;
    let player = OneBurst {
        left: frame,
        files: Vec::new(),
    };
    let mut emu = with_script(MusicEmu::from_player(
        Box::new(player),
        "".into(),
        44100.0,
        2,
    ));

    // The trace sits on the baseline while the window holds silence, and
    // jumps to the top of the band while the burst is passing through.
    let burst_on_screen = |emu: &MusicEmu| {
        let band = HEIGHT / 2;
        emu.frame[..band * WIDTH / 2].contains(&LEFT_TRACE)
    };

    // Whole frames of delay: the window moves a frame at a time, so the
    // burst lands in the one after that many have gone by.
    let expected = 1 + (44100.0 * SCOPE_DELAY) as usize * 2 / frame;
    let mut seen: Option<usize> = None;
    for n in 1..expected + 4 {
        emu.run();
        if burst_on_screen(&emu) {
            seen.get_or_insert(n);
        } else {
            assert!(
                seen.is_none() || n > expected,
                "the burst left the scope early, at frame {n}"
            );
        }
    }
    let seen = seen.expect("the burst never reached the scope");
    assert!(
        seen.abs_diff(expected) <= 1,
        "the burst showed at frame {seen}, expected about {expected}"
    );
}

/// A stereo player is passed through untouched.
#[test]
fn stereo_output_is_not_doubled() {
    let (mut emu, _) = fake_with_channels(0, false, 2);
    assert!(emu.run());
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert_eq!(samples.len(), 44100 / FRAME_RATE as usize * 2);
    assert!(samples.iter().all(|&s| s == 1000));
}

/// Every read must be a whole [`CHUNK`], never the frame-sized sliver that
/// makes the UADE plugin return silence forever.
#[test]
fn the_player_is_read_in_chunks() {
    let (mut emu, sizes) = fake(0, false);
    for _ in 0..30 {
        emu.run();
    }
    let sizes = sizes.lock().unwrap();
    assert!(!sizes.is_empty(), "the player was never read");
    assert!(
        sizes.iter().all(|&n| n == CHUNK),
        "reads were not all CHUNK-sized: {sizes:?}"
    );
}

/// A player that has not started yet must not be mistaken for one that has
/// finished, however many frames the warm-up takes.
#[test]
fn warm_up_silence_is_not_the_end() {
    let (mut emu, _) = fake(EMPTY_READS_UNTIL_END - 1, false);
    for _ in 0..EMPTY_READS_UNTIL_END - 1 {
        emu.run();
        assert!(!emu.is_idle(), "gave up during warm-up");
    }
    // Audio arrives once the player wakes up, and none was lost on the way.
    emu.run();
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert_eq!(samples.len(), 44100 / FRAME_RATE as usize * 2);
    assert!(!emu.is_idle());
}

/// Silence that does not end, on the other hand, is a finished song — which
/// is what the frontend's idle timeout waits for.
#[test]
fn sustained_silence_ends_the_song() {
    let (mut emu, _) = fake(0, true);
    for _ in 0..EMPTY_READS_UNTIL_END {
        emu.run();
    }
    assert!(emu.is_idle(), "song never reported as finished");

    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert!(samples.is_empty(), "silence produced audio");
}

/// The `musix` data directory in this checkout. Only some formats need it,
/// and the module used here is not one of them, so a missing directory is
/// fine — [`init_musix`] warns and carries on.
fn data_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("system/musix")
}

/// A module of this test's own, in a directory that goes away with the
/// returned handle. Every test needs its own: these run in parallel, and a
/// shared path made them race — `fs::write` truncates, so one test writing
/// the module handed another an empty file to play, and the cleanup at the
/// end of one deleted the song another was still loading.
///
/// A directory rather than a `NamedTempFile` because the plugin match and
/// the secondary-file lookup both key off the extension, which must stay
/// `.mod`.
fn test_song() -> (TempDir, PathBuf) {
    let dir = tempdir();
    let path = dir.path().join("song.mod");
    write_test_mod(&path);
    (dir, path)
}

/// The temp directory the tests build their files in. Named for what it is
/// so a leftover is recognisable, and removed when the test ends.
fn tempdir() -> TempDir {
    tempfile::Builder::new()
        .prefix("music_emu_test-")
        .tempdir()
        .expect("temp dir")
}

/// An unpacked archive arrives as a directory. The song played is the first
/// one `musix` recognises, in name order, and the files around it — the
/// `.nfo`, the scroll text, the unrelated subdirectory — are passed over.
#[test]
fn a_directory_plays_its_first_song() {
    let tmp = tempdir();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("extras")).unwrap();
    // Named so the junk sorts first: the pick must be about what plays, not
    // about what comes first in the directory.
    std::fs::write(dir.join("aaa_file_id.diz"), b"just a text file").unwrap();
    write_test_mod(&dir.join("m_second.mod"));
    write_test_mod(&dir.join("b_first.mod"));
    write_test_mod(&dir.join("extras/nested.mod"));

    assert!(can_handle(dir, &data_dir()));
    assert_eq!(playable_file(dir), Some(dir.join("b_first.mod")));

    // And it really loads through the directory path.
    let mut emu = MusicEmu::new(dir, &data_dir(), None).unwrap();
    assert!(emu.run());
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert!(samples.iter().any(|&s| s != 0), "directory load is silent");
}

/// Nothing playable at the top level: the subdirectories are searched too,
/// which is what a release packed as `Group-Demo/music/*.mod` needs.
#[test]
fn a_song_in_a_subdirectory_is_found() {
    let tmp = tempdir();
    let dir = tmp.path();
    std::fs::create_dir_all(dir.join("music")).unwrap();
    std::fs::write(dir.join("readme.txt"), b"nothing to play here").unwrap();
    write_test_mod(&dir.join("music/tune.mod"));

    // Through `can_handle` so the plugins are registered: `playable_file`
    // sees nothing playable until `init_musix` has run.
    assert!(can_handle(dir, &data_dir()));
    assert_eq!(playable_file(dir), Some(dir.join("music/tune.mod")));
}

/// A directory with nothing playable in it is not this backend's business,
/// so `create_core` must not be told otherwise.
#[test]
fn a_directory_without_music_is_rejected() {
    let tmp = tempdir();
    let dir = tmp.path();
    std::fs::write(dir.join("readme.txt"), b"nothing to play here").unwrap();

    assert!(!can_handle(dir, &data_dir()));
    assert!(MusicEmu::new(dir, &data_dir(), None).is_err());
}

/// Two songs may be alive at once — the frontend builds the next one before
/// letting go of the one playing — and no format may take that as a reason
/// to refuse. SNDH is the one that did: `musix`'s sc68 plugin used to claim
/// libsc68's process-wide init per song, so the second SNDH found the
/// library already initialised, failed to load, and came back as "no plugin
/// for file" while every other format was fine.
#[test]
fn a_second_sndh_loads_while_the_first_is_playing() {
    let song = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/music/Pushover.sndh");
    let mut first = MusicEmu::new(&song, &data_dir(), None).expect("first SNDH");
    let mut second = MusicEmu::new(&song, &data_dir(), None).expect("second SNDH");

    // And neither is left mute by the other's existence: tearing one down
    // must not pull the library out from under the other, either.
    let plays = |emu: &mut MusicEmu| {
        let mut samples = Vec::new();
        for _ in 0..8 {
            emu.run();
            emu.with_audio(&mut |s| samples.extend_from_slice(s));
        }
        samples.iter().any(|&s| s != 0)
    };
    assert!(plays(&mut first), "the first SNDH is silent");
    drop(first);
    assert!(plays(&mut second), "the second SNDH is silent");
}

#[test]
fn renders_audio_and_a_scope() {
    let (_tmp, song) = test_song();
    assert!(can_handle(&song, &data_dir()), "musix rejected the module");

    let mut emu = MusicEmu::new(&song, &data_dir(), Some(&script())).unwrap();
    assert!(emu.sample_rate() > 0.0);
    assert_eq!(emu.get_frame_size(), (WIDTH, HEIGHT));

    // One frame of audio, handed over exactly once.
    assert!(emu.run());
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    let expected = (emu.sample_rate() / FRAME_RATE) as usize * 2;
    assert_eq!(samples.len(), expected, "wrong frame length");
    assert!(samples.iter().any(|&s| s != 0), "rendered frame is silent");
    assert!(!emu.is_idle());

    // The buffer is drained by the collection above, not re-delivered.
    let mut again = Vec::new();
    emu.with_audio(&mut |s| again.extend_from_slice(s));
    assert!(again.is_empty(), "audio delivered twice");

    // The scope was drawn from those samples, and moves as they do.
    let mut first = Vec::new();
    emu.with_frame(&mut |w, h, frame| {
        assert_eq!((w, h), (WIDTH, HEIGHT));
        assert_eq!(frame.len(), w * h);
        assert!(frame.contains(&LEFT_TRACE), "no waveform drawn");
        first = frame.to_vec();
    });
    let hash = emu.frame_hash();
    assert!(emu.run());
    assert_ne!(emu.frame_hash(), hash, "frame hash did not move");
}

/// A song with no script still plays. The picture is blank rather than
/// stale or garbage, and nothing about the audio path notices.
#[test]
fn a_song_without_a_script_still_plays() {
    let (_tmp, song) = test_song();
    let mut emu = MusicEmu::new(&song, &data_dir(), None).unwrap();

    assert!(emu.run());
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert!(samples.iter().any(|&s| s != 0), "no audio without a script");

    emu.with_frame(&mut |_, _, frame| {
        assert!(
            frame.iter().all(|&px| px == BLANK),
            "something was drawn without a script"
        );
    });
}

/// A script that cannot be loaded is not a load failure for the song — the
/// same call `init_musix` makes about a missing data directory.
#[test]
fn a_broken_script_does_not_stop_the_song() {
    let (tmp, song) = test_song();
    // Inside this test's own directory, so it is missing because nothing
    // ever wrote it rather than because another test cleaned it up.
    let missing = tmp.path().join("no_such_script.lua");

    let mut emu = MusicEmu::new(&song, &data_dir(), Some(&missing))
        .expect("a missing script must not fail the load");
    assert!(emu.run());
    let mut samples = Vec::new();
    emu.with_audio(&mut |s| samples.extend_from_slice(s));
    assert!(
        samples.iter().any(|&s| s != 0),
        "a bad script muted the song"
    );
}

/// The frame length must average out to the sample rate over time, however
/// the per-frame count rounds.
#[test]
fn frame_lengths_track_the_sample_rate() {
    let (_tmp, song) = test_song();
    let mut emu = MusicEmu::new(&song, &data_dir(), None).unwrap();

    for _ in 0..FRAME_RATE as usize {
        emu.run();
    }
    let mut total = 0;
    emu.with_audio(&mut |s| total += s.len());
    // One second of stereo, within a single frame's rounding.
    let expected = emu.sample_rate() as usize * 2;
    assert!(
        total.abs_diff(expected) <= 2,
        "a second of audio was {total} samples, expected ~{expected}"
    );
}
