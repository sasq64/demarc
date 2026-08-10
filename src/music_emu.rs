//! Music backend for the chiptune and tracker formats handled by the `musix`
//! library (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, PSF, AHX, TFMX, …).
//!
//! [`MusicEmu`] implements [`Backend`] so a bare music file slots into the same
//! frontend plumbing as a libretro core: [`run`](Backend::run) renders one
//! frame's worth of interleaved i16 stereo, which [`with_audio`](Backend::with_audio)
//! hands to the audio sink, and each frame also draws an oscilloscope of those
//! very samples so there is something on screen — a music file has no video of
//! its own, and a black window looks like a failed load.
//!
//! Unlike the libretro and Flash backends this one needs no worker thread: a
//! frame of chip audio costs microseconds to render, so it is generated inline
//! on the frontend's thread, driven by the same [`FRAME_RATE`] pacing the
//! frontend already applies to every other core.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use musix::MusixPlayer;
use tracing::{debug, info, warn};

use crate::retro_emu::Backend;

/// Rate the frontend paces [`run`](Backend::run) at. Nothing in the music
/// formats themselves is tied to it — it only sets how much audio is rendered
/// per call, and so how often the scope is redrawn.
const FRAME_RATE: f64 = 60.0;

/// Size of the oscilloscope frame. 4:3 to match the machines this music came
/// from, and small enough that clearing and redrawing it every frame costs
/// nothing next to the audio rendering.
const WIDTH: usize = 320;
const HEIGHT: usize = 240;

/// Pack a colour the way the frontend expects it: one `u32` per pixel whose
/// native bytes are `[r, g, b, a]` (see [`crate::retro_emu::frame_bytes`]).
const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    u32::from_ne_bytes([r, g, b, 255])
}

/// How many samples to ask the player for at a time. Not a frame's worth: the
/// UADE plugin (AHX, TFMX, Hippel, FC — most of the Amiga formats) answers a
/// request smaller than its own message size with nothing at all, forever, so
/// asking for one 60Hz frame (1470 samples at 44.1kHz) yields silence. Reading
/// in blocks and handing out frames from the block keeps every plugin happy;
/// 8192 samples is ~93ms of stereo, which is well past the threshold and still
/// small enough not to matter for latency.
const CHUNK: usize = 8192;

/// How many consecutive empty reads mean the song is over rather than that the
/// player has not started yet. UADE's Amiga emulation returns nothing for the
/// first few reads while it boots the replayer, so the first empty read cannot
/// be taken at face value. At one attempt per frame this is ~0.5s.
const EMPTY_READS_UNTIL_END: u32 = 32;

/// Ceiling on audio rendered but not yet collected — about a second of stereo.
/// See the note in [`Backend::run`].
const MAX_QUEUED_AUDIO: usize = 96000;

const BACKGROUND: u32 = rgb(0x08, 0x08, 0x10);
/// Baseline drawn at the vertical centre of each channel's band.
const AXIS: u32 = rgb(0x20, 0x20, 0x30);
const LEFT_TRACE: u32 = rgb(0x50, 0xe0, 0xa0);
const RIGHT_TRACE: u32 = rgb(0xe0, 0x90, 0x50);

/// Initialise `musix` exactly once per process, against the data directory that
/// holds the bits some plugins need at runtime (UADE's players, the sc68
/// replayers, `adplug.db`, …).
///
/// The result is cached because `musix::init` registers a process-global plugin
/// list: calling it again is a no-op upstream, so a failure would otherwise be
/// reported only for the first song loaded and silently swallowed for the rest.
fn init_musix(data_dir: &Path) -> Result<()> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        if !data_dir.is_dir() {
            // Not fatal: the formats whose plugins are self-contained (SID, MOD,
            // NSF, …) play fine without it. Only the data-driven ones — UADE,
            // sc68, AdLib — need the directory to exist.
            warn!("musix data dir {data_dir:?} not found; some formats may not play");
        }
        info!("Initializing musix with data dir {data_dir:?}");
        musix::init(data_dir).map_err(|e| e.msg)
    })
    .clone()
    .map_err(|e| anyhow!("Could not initialize musix: {e}"))
}

/// How far to descend when looking for a song inside a directory. An unpacked
/// release is a handful of levels at most; the limit is really there so a
/// symlink loop cannot walk forever.
const MAX_SCAN_DEPTH: u32 = 4;

/// The file to actually play for `song`, which the frontend may hand over as
/// either a music file or a directory: archives are unpacked before a backend
/// sees them, and one holding several songs (or songs plus a `file_id.diz`)
/// has nothing to single out, so `prepare_file` passes the directory along.
///
/// For a directory this is the first file `musix` can play, in name order,
/// with the top level preferred over any subdirectory. Requires [`init_musix`]
/// to have run: with no plugins registered every file looks unplayable.
fn playable_file(song: &Path) -> Option<PathBuf> {
    if !song.is_dir() {
        return musix::can_handle(song)
            .unwrap_or(false)
            .then(|| song.to_path_buf());
    }
    find_song(song, MAX_SCAN_DEPTH)
}

fn find_song(dir: &Path, depth: u32) -> Option<PathBuf> {
    if depth == 0 {
        return None;
    }
    let (mut dirs, mut files): (Vec<PathBuf>, Vec<PathBuf>) = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .partition(|path| path.is_dir());

    // Lowercased so the pick does not depend on the platform's idea of case
    // order — the same archive must yield the same song everywhere.
    let by_name = |path: &PathBuf| {
        path.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    };
    files.sort_by_key(by_name);
    dirs.sort_by_key(by_name);

    files
        .into_iter()
        .find(|file| musix::can_handle(file).unwrap_or(false))
        .or_else(|| dirs.iter().find_map(|dir| find_song(dir, depth - 1)))
}

/// Whether `musix` can play `song` — a music file, or a directory holding one
/// (see [`playable_file`]). Cheap enough to call while working out what a file
/// is: it only sniffs headers, it does not decode.
pub fn can_handle(song: &Path, data_dir: &Path) -> bool {
    if init_musix(data_dir).is_err() {
        return false;
    }
    playable_file(song).is_some()
}

/// A loaded song, rendered a frame at a time.
pub struct MusicEmu {
    player: Box<dyn MusixPlayer>,
    /// Output rate the player generates at, in Hz.
    sample_rate: f64,
    /// Interleaved stereo samples rendered but not yet collected by
    /// [`with_audio`](Backend::with_audio).
    audio: Vec<i16>,
    /// Fractional sample carried between frames, so a rate that is not a whole
    /// multiple of [`FRAME_RATE`] (44100/60 = 735 exactly, but 48000/50 is not)
    /// neither drifts ahead of nor falls behind real time.
    sample_debt: f64,
    /// Samples read from the player in [`CHUNK`]-sized blocks but not yet
    /// handed out as frames, from [`Self::pending_pos`] onwards.
    pending: Vec<i16>,
    pending_pos: usize,
    /// Consecutive reads that returned nothing; see [`EMPTY_READS_UNTIL_END`].
    empty_reads: u32,
    /// The current frame's samples — what the scope draws, and what the last
    /// [`run`](Backend::run) added to [`Self::audio`].
    frame_samples: Vec<i16>,
    /// The oscilloscope image handed to the frontend.
    frame: Vec<u32>,
    /// Bumped on every redraw. See [`Backend::frame_hash`].
    serial: u64,
    /// Set once the player has stopped producing samples for long enough to
    /// call it over. Drives [`is_idle`](Backend::is_idle) so the frontend can
    /// move on to the next entry.
    ended: bool,
}

// Safety: every player `musix::load_song` can return (`ChipPlayer`, wrapping an
// opaque C++ player, and `FlacPlayer`) is declared `Send + Sync` upstream; only
// the `Box<dyn MusixPlayer>` erasure loses that, since the trait itself carries
// no bounds. `MusicEmu` adds nothing but plain buffers on top, and the frontend
// needs the `Send + Sync` for the Bevy component that owns the backend.
unsafe impl Send for MusicEmu {}
unsafe impl Sync for MusicEmu {}

impl MusicEmu {
    /// Load `song`, with `data_dir` naming the `musix` data directory (see
    /// [`init_musix`]).
    pub fn new(song: &Path, data_dir: &Path) -> Result<Self> {
        init_musix(data_dir)?;
        let file = playable_file(song)
            .ok_or_else(|| anyhow!("No music file musix can play in {}", song.display()))?;
        if file != song {
            info!("Playing {file:?} out of {song:?}");
        }
        let mut player = musix::load_song(&file)
            .map_err(|e| anyhow!("Could not play {}: {}", file.display(), e.msg))?;

        // Only informational — the frontend takes its title/composer from the
        // release db — but it is the one place a wrong plugin match shows up.
        let meta = |player: &mut Box<dyn MusixPlayer>, what: &str| {
            player.get_meta_string(what).unwrap_or_default()
        };
        info!(
            "Playing '{}' by '{}' [{}]",
            meta(&mut player, "title"),
            meta(&mut player, "composer"),
            meta(&mut player, "format")
        );

        // A player that reports no rate would make the frame length zero, and
        // the song would never advance; 44.1kHz is what every musix plugin
        // resamples to by default.
        let sample_rate = match player.get_frequency() {
            0 => 44100.0,
            hz => hz as f64,
        };

        Ok(Self::from_player(player, sample_rate))
    }

    fn from_player(player: Box<dyn MusixPlayer>, sample_rate: f64) -> Self {
        Self {
            player,
            sample_rate,
            audio: Vec::new(),
            sample_debt: 0.0,
            pending: Vec::new(),
            pending_pos: 0,
            empty_reads: 0,
            frame_samples: Vec::new(),
            frame: vec![BACKGROUND; WIDTH * HEIGHT],
            serial: 1,
            ended: false,
        }
    }

    /// Top [`Self::pending`] up to at least `wanted` unread samples, reading
    /// from the player in [`CHUNK`]-sized blocks. Gives up for this call as
    /// soon as a read comes back empty, so a player that has stopped (or has
    /// not started) costs one read per frame rather than a spin.
    fn fill_pending(&mut self, wanted: usize) {
        while self.pending.len() - self.pending_pos < wanted {
            // Drop what has already been handed out before growing the buffer,
            // so it stays a chunk or two long instead of the whole song.
            if self.pending_pos > 0 {
                self.pending.drain(..self.pending_pos);
                self.pending_pos = 0;
            }
            let base = self.pending.len();
            self.pending.resize(base + CHUNK, 0);
            let got = self.player.get_samples(&mut self.pending[base..]);
            self.pending.truncate(base + got);

            if got == 0 {
                self.empty_reads += 1;
                self.ended = self.empty_reads >= EMPTY_READS_UNTIL_END;
                return;
            }
            self.empty_reads = 0;
            self.ended = false;
        }
    }

    /// Take the next frame's worth of audio into [`Self::frame_samples`],
    /// returning how many samples (not sample pairs) were available.
    fn render_frame(&mut self) -> usize {
        self.sample_debt += self.sample_rate / FRAME_RATE;
        // Interleaved stereo, so the count is always even: a partial pair would
        // swap the channels for the rest of the song.
        let pairs = self.sample_debt as usize;
        self.sample_debt -= pairs as f64;
        let wanted = pairs * 2;

        self.fill_pending(wanted);

        // A short read at the end of a song can leave an odd number of samples;
        // rounding down keeps left and right where they belong.
        let avail = (self.pending.len() - self.pending_pos).min(wanted) & !1;
        self.frame_samples.clear();
        self.frame_samples
            .extend_from_slice(&self.pending[self.pending_pos..self.pending_pos + avail]);
        self.pending_pos += avail;

        // Subsong changes and streamed titles arrive as meta updates while the
        // song plays; nothing consumes them yet, but leaving them unread lets
        // the queue grow for the whole playback.
        while let Some(what) = self.player.get_changed_meta() {
            debug!("Meta changed: {what}");
        }

        avail
    }

    /// Draw [`Self::frame_samples`] as a two-channel oscilloscope: left in the
    /// top half of the frame, right in the bottom. Consecutive samples are
    /// joined by a vertical span so a fast waveform reads as a continuous trace
    /// rather than a dotted line.
    fn draw_scope(&mut self) {
        self.frame.fill(BACKGROUND);
        let band = HEIGHT / 2;
        let half = band as i32 / 2;

        for (channel, colour) in [(0usize, LEFT_TRACE), (1usize, RIGHT_TRACE)] {
            let top = channel * band;
            // Baseline first, so the trace draws over it.
            for x in 0..WIDTH {
                self.frame[(top + half as usize) * WIDTH + x] = AXIS;
            }

            let pairs = self.frame_samples.len() / 2;
            if pairs == 0 {
                continue;
            }

            // One column per horizontal pixel, sampling the frame's waveform at
            // even intervals. A frame is ~735 samples against 320 columns, so
            // this decimates; the exact peaks matter less than the shape.
            let mut prev: Option<i32> = None;
            for x in 0..WIDTH {
                let idx = x * pairs / WIDTH * 2 + channel;
                let sample = self.frame_samples[idx] as i32;
                // i16 range -> half the band, centred on the baseline.
                let y = half - (sample * half / i16::MAX as i32);
                let y = y.clamp(0, band as i32 - 1);
                let from = prev.unwrap_or(y).min(y);
                let to = prev.unwrap_or(y).max(y);
                for py in from..=to {
                    self.frame[(top + py as usize) * WIDTH + x] = colour;
                }
                prev = Some(y);
            }
        }
        self.serial += 1;
    }
}

impl Backend for MusicEmu {
    fn run(&mut self) -> bool {
        if self.render_frame() == 0 {
            // No audio this frame: either the player has not started yet, or
            // the song is over. Either way the last scope stays on screen — a
            // sudden flat line would read as a decoding fault — and the frontend
            // is told about the end through `is_idle`.
            return true;
        }
        self.audio.extend_from_slice(&self.frame_samples);
        // Nothing normally accumulates here — the frontend collects after every
        // `run` — but `--speed-test` steps the core as fast as it can and never
        // reads the audio back, so drop the oldest instead of growing forever.
        if self.audio.len() > MAX_QUEUED_AUDIO {
            let excess = self.audio.len() - MAX_QUEUED_AUDIO;
            self.audio.drain(..excess);
        }
        self.draw_scope();
        true
    }

    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.audio);
        self.audio.clear();
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(WIDTH, HEIGHT, &self.frame);
    }

    fn frame_hash(&self) -> u64 {
        self.serial
    }

    fn get_frame_size(&self) -> (usize, usize) {
        (WIDTH, HEIGHT)
    }

    fn aspect_ratio(&self) -> f32 {
        WIDTH as f32 / HEIGHT as f32
    }

    fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    fn fps(&self) -> f64 {
        FRAME_RATE
    }

    /// True once the song has run out, which is what the frontend's idle
    /// timeout watches to advance to the next entry.
    fn is_idle(&self) -> bool {
        self.ended
    }

    /// Fast-forward by rendering and discarding whole frames. Cheap for chip
    /// formats, and the only way to skip: seeking is optional in `musix` and
    /// most plugins do not implement it.
    fn skip_frames(&mut self, frames: u32) {
        for _ in 0..frames {
            if self.render_frame() == 0 && self.ended {
                break;
            }
        }
        self.frame_samples.clear();
    }

    /// Restart from the beginning of the first subsong. Plugins that do not
    /// implement seeking ignore this.
    fn reset(&mut self) {
        self.player.seek(0, 0);
        self.audio.clear();
        self.pending.clear();
        self.pending_pos = 0;
        self.empty_reads = 0;
        self.ended = false;
    }

    // A song has no disks, no keyboard, no joypad and no mouse.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        0
    }
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

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

    fn fake(empty_reads: u32, always_empty: bool) -> (MusicEmu, Arc<Mutex<Vec<usize>>>) {
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let player = FakePlayer {
            empty_reads,
            reads: 0,
            always_empty,
            sizes: sizes.clone(),
            files: Vec::new(),
        };
        (MusicEmu::from_player(Box::new(player), 44100.0), sizes)
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

    /// Build a minimal but valid 4-channel ProTracker module: one instrument
    /// holding a loud square wave, and a pattern that plays it on channel 1.
    /// Generating the file keeps the test self-contained — no binary asset, and
    /// nothing borrowed from the `musix` checkout.
    fn write_test_mod(path: &Path) {
        const SAMPLE_WORDS: usize = 16;
        let mut m = Vec::new();
        m.extend_from_slice(&[0u8; 20]); // song title
        for i in 0..31 {
            m.extend_from_slice(&[0u8; 22]); // sample name
            // Length in words, finetune, volume, loop start/length in words.
            // Only the first sample carries any data; a loop covering the whole
            // sample keeps it sounding for the note's full length.
            let (len, vol, loop_len) = if i == 0 {
                (SAMPLE_WORDS as u16, 64u8, SAMPLE_WORDS as u16)
            } else {
                (0, 0, 1)
            };
            m.extend_from_slice(&len.to_be_bytes());
            m.push(0);
            m.push(vol);
            m.extend_from_slice(&0u16.to_be_bytes());
            m.extend_from_slice(&loop_len.to_be_bytes());
        }
        m.push(1); // song length in patterns
        m.push(127); // restart position (unused)
        let mut order = [0u8; 128];
        order[0] = 0;
        m.extend_from_slice(&order);
        m.extend_from_slice(b"M.K.");

        // 64 rows x 4 channels x 4 bytes. Row 0 plays sample 1 at period 428
        // (C-2) on the first channel; every other slot stays empty. The sample
        // number is split across two nibbles: the high one shares a byte with
        // the period's top bits, the low one shares a byte with the effect.
        let mut pattern = vec![0u8; 64 * 4 * 4];
        let period: u16 = 428;
        let sample: u8 = 1;
        pattern[0] = (sample & 0xf0) | ((period >> 8) as u8 & 0x0f);
        pattern[1] = (period & 0xff) as u8;
        pattern[2] = sample << 4;
        m.extend_from_slice(&pattern);

        // Sample data: a full-scale square wave.
        for i in 0..SAMPLE_WORDS * 2 {
            m.push(if (i / 4) % 2 == 0 { 127 } else { 129 });
        }

        std::fs::write(path, m).unwrap();
    }

    /// The `musix` data directory in this checkout. Only some formats need it,
    /// and the module used here is not one of them, so a missing directory is
    /// fine — [`init_musix`] warns and carries on.
    fn data_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("system/musix")
    }

    fn test_song() -> std::path::PathBuf {
        // Not `tempfile`: the plugin match and the secondary-file lookup both
        // key off the extension, which must stay `.mod`.
        let path = std::env::temp_dir().join("music_emu_test.mod");
        write_test_mod(&path);
        path
    }

    /// An unpacked archive arrives as a directory. The song played is the first
    /// one `musix` recognises, in name order, and the files around it — the
    /// `.nfo`, the scroll text, the unrelated subdirectory — are passed over.
    #[test]
    fn a_directory_plays_its_first_song() {
        let dir = std::env::temp_dir().join("music_emu_dir_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("extras")).unwrap();
        // Named so the junk sorts first: the pick must be about what plays, not
        // about what comes first in the directory.
        std::fs::write(dir.join("aaa_file_id.diz"), b"just a text file").unwrap();
        write_test_mod(&dir.join("m_second.mod"));
        write_test_mod(&dir.join("b_first.mod"));
        write_test_mod(&dir.join("extras/nested.mod"));

        assert!(can_handle(&dir, &data_dir()));
        assert_eq!(playable_file(&dir), Some(dir.join("b_first.mod")));

        // And it really loads through the directory path.
        let mut emu = MusicEmu::new(&dir, &data_dir()).unwrap();
        assert!(emu.run());
        let mut samples = Vec::new();
        emu.with_audio(&mut |s| samples.extend_from_slice(s));
        assert!(samples.iter().any(|&s| s != 0), "directory load is silent");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole route a packed release takes: `prepare_file` unpacks the
    /// archive, finds nothing it recognises inside, and hands `create_core` the
    /// temp *directory* with no system type — which is exactly where this
    /// backend picks the music up.
    #[test]
    fn a_zipped_release_reaches_the_backend() {
        use crate::files::{collect_file, prepare_file};
        use crate::systems::SystemType;
        use std::collections::HashMap;

        let src = std::env::temp_dir().join("music_emu_zip_src");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).unwrap();
        write_test_mod(&src.join("b_first.mod"));
        write_test_mod(&src.join("m_second.mod"));

        let zip_path = std::env::temp_dir().join("music_emu_songs.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        for name in ["b_first.mod", "m_second.mod"] {
            zip.start_file(name, opts).unwrap();
            std::io::Write::write_all(&mut zip, &std::fs::read(src.join(name)).unwrap()).unwrap();
        }
        // A release is more than its music; the junk must not confuse the pick.
        zip.start_file("file_id.diz", opts).unwrap();
        std::io::Write::write_all(&mut zip, b"scene release").unwrap();
        zip.finish().unwrap();

        let work = prepare_file(&collect_file(&zip_path).unwrap(), &HashMap::new()).unwrap();
        assert!(work.path.is_dir(), "expected an unpacked directory");
        assert_eq!(work.system_type, SystemType::Unknown);
        assert!(can_handle(&work.path, &data_dir()));

        let mut emu = MusicEmu::new(&work.path, &data_dir()).unwrap();
        assert!(emu.run());
        let mut samples = Vec::new();
        emu.with_audio(&mut |s| samples.extend_from_slice(s));
        assert!(samples.iter().any(|&s| s != 0), "zipped release is silent");

        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_file(&zip_path);
    }

    /// Nothing playable at the top level: the subdirectories are searched too,
    /// which is what a release packed as `Group-Demo/music/*.mod` needs.
    #[test]
    fn a_song_in_a_subdirectory_is_found() {
        let dir = std::env::temp_dir().join("music_emu_nested_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("music")).unwrap();
        std::fs::write(dir.join("readme.txt"), b"nothing to play here").unwrap();
        write_test_mod(&dir.join("music/tune.mod"));

        assert_eq!(playable_file(&dir), Some(dir.join("music/tune.mod")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A directory with nothing playable in it is not this backend's business,
    /// so `create_core` must not be told otherwise.
    #[test]
    fn a_directory_without_music_is_rejected() {
        let dir = std::env::temp_dir().join("music_emu_empty_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("readme.txt"), b"nothing to play here").unwrap();

        assert!(!can_handle(&dir, &data_dir()));
        assert!(MusicEmu::new(&dir, &data_dir()).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn renders_audio_and_a_scope() {
        let song = test_song();
        assert!(can_handle(&song, &data_dir()), "musix rejected the module");

        let mut emu = MusicEmu::new(&song, &data_dir()).unwrap();
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
            assert!(
                frame.iter().any(|&px| px == LEFT_TRACE),
                "no waveform drawn"
            );
            first = frame.to_vec();
        });
        let hash = emu.frame_hash();
        assert!(emu.run());
        assert_ne!(emu.frame_hash(), hash, "frame hash did not move");

        let _ = std::fs::remove_file(&song);
    }

    /// The frame length must average out to the sample rate over time, however
    /// the per-frame count rounds.
    #[test]
    fn frame_lengths_track_the_sample_rate() {
        let song = test_song();
        let mut emu = MusicEmu::new(&song, &data_dir()).unwrap();

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

        let _ = std::fs::remove_file(&song);
    }
}


