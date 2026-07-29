use anyhow::{Result, bail};
use tracing::{debug, info, warn};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use tempfile::TempDir;
use unarc_rs::unified::ArchiveFormat;

use crate::systems::{SystemType, get_system_type};

fn check_reset_vector(data: &[u8]) -> bool {
    let len = data.len();
    if len < 4 {
        return false;
    }

    // Reset vector is at the last 4 bytes: NMI, RESET, IRQ/BRK
    // For a cart ending at $FFFF, reset vector is at offset len-4+2
    let reset_lo = data[len - 4 + 2] as u16;
    let reset_hi = data[len - 4 + 3] as u16;
    let reset_addr = (reset_hi << 8) | reset_lo;

    // Should point into the high ROM bank, typically $F000–$FFFF
    // (or $E000–$FFFF for 8KB, etc.)
    let bank_start = 0x10000u32 - len as u32;
    reset_addr as u32 >= bank_start
}

pub fn is_disk_image(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|p| p.to_str()) {
        let ext = ext.to_lowercase();
        return [
            "d64", "d81", "adf", "dms", "msa", "st", "atr", "xex", "cue", "chd",
        ]
        .contains(&ext.as_str());
    }
    false
}

/// CD audio is 44.1 kHz 16-bit stereo; a cue's audio tracks must match, because
/// the core seeks past the WAV header and reads the rest as raw PCM.
const CDDA_RATE: u32 = 44100;

/// One `FILE "<name>" <kind>` line from a cue sheet.
struct CueFile<'a> {
    line: &'a str,
    name: String,
    kind: String,
}

/// Pull the `FILE` lines out of a cue sheet. Handles both quoted and bare names
/// (scene sheets use either); the kind is always the last token on the line.
fn parse_cue_files(text: &str) -> Vec<CueFile<'_>> {
    let mut out = vec![];
    for line in text.lines() {
        let rest = match line.trim().strip_prefix("FILE ") {
            Some(rest) => rest.trim(),
            None => continue,
        };
        let (name, kind) = if let Some(after) = rest.strip_prefix('"') {
            match after.split_once('"') {
                Some((name, kind)) => (name.to_string(), kind.trim().to_string()),
                None => continue,
            }
        } else {
            match rest.rsplit_once(char::is_whitespace) {
                Some((name, kind)) => (name.trim().to_string(), kind.trim().to_string()),
                None => continue,
            }
        };
        out.push(CueFile { line, name, kind });
    }
    out
}

/// Decode an MP3 to 44.1 kHz 16-bit stereo and write it as a WAV.
fn transcode_mp3_to_wav(src: &Path, dest: &Path) -> Result<()> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = fs::File::open(src)?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");
    let probed = symphonia::default::get_probe().format(
        &hint,
        stream,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| anyhow::anyhow!("no audio track in {src:?}"))?;
    let track_id = track.id;
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let mut pcm: Vec<i16> = Vec::new();
    let mut rate = CDDA_RATE;
    let mut channels = 2usize;
    let mut buf: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Symphonia signals end-of-stream as an IO error.
            Err(symphonia::core::errors::Error::IoError(_)) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A truncated or slightly corrupt frame shouldn't lose the track.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        channels = spec.channels.count();
        let sbuf =
            buf.get_or_insert_with(|| SampleBuffer::<i16>::new(decoded.capacity() as u64, spec));
        sbuf.copy_interleaved_ref(decoded);
        pcm.extend_from_slice(sbuf.samples());
    }

    // Force the shape CD audio requires. Both are no-ops for the 44.1 kHz stereo
    // MP3s scene discs actually ship, but silently wrong speed or a half-length
    // track is a nasty way to find out otherwise.
    if channels == 1 {
        warn!("{src:?} is mono; duplicating to stereo for CD audio");
        pcm = pcm.iter().flat_map(|&s| [s, s]).collect();
        channels = 2;
    } else if channels > 2 {
        warn!("{src:?} has {channels} channels; keeping the first two");
        pcm = pcm.chunks(channels).flat_map(|f| [f[0], f[1]]).collect();
        channels = 2;
    }
    if rate != CDDA_RATE {
        warn!("{src:?} is {rate} Hz; resampling to {CDDA_RATE} Hz for CD audio");
        let frames = pcm.len() / channels;
        let out_frames = (frames as u64 * CDDA_RATE as u64 / rate.max(1) as u64) as usize;
        let mut out = Vec::with_capacity(out_frames * 2);
        for i in 0..out_frames {
            let pos = i as f64 * rate as f64 / CDDA_RATE as f64;
            let idx = (pos as usize).min(frames.saturating_sub(1));
            out.push(pcm[idx * 2]);
            out.push(pcm[idx * 2 + 1]);
        }
        pcm = out;
    }

    write_wav(dest, &pcm)
}

/// Write interleaved 16-bit stereo samples as a canonical 44-byte-header WAV.
fn write_wav(dest: &Path, pcm: &[i16]) -> Result<()> {
    const _: () = assert!(cfg!(target_endian = "little"), "write_wav assumes LE");

    use std::io::Write;
    let data_len = (pcm.len() * 2) as u32;
    let byte_rate = CDDA_RATE * 2 * 2;

    let mut header = [0u8; 44];
    let mut put = |at: usize, bytes: &[u8]| header[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"RIFF");
    put(4, &(36 + data_len).to_le_bytes());
    put(8, b"WAVEfmt ");
    put(16, &16u32.to_le_bytes()); // PCM fmt chunk size
    put(20, &1u16.to_le_bytes()); // PCM
    put(22, &2u16.to_le_bytes()); // stereo
    put(24, &CDDA_RATE.to_le_bytes());
    put(28, &byte_rate.to_le_bytes());
    put(32, &4u16.to_le_bytes()); // block align
    put(34, &16u16.to_le_bytes()); // bits per sample
    put(36, b"data");
    put(40, &data_len.to_le_bytes());

    // SAFETY: `i16` has no padding or invalid bit patterns, and `u8`'s alignment
    // is weaker, so any `[i16]` is also a valid `[u8]` of twice the length.
    let samples = unsafe { std::slice::from_raw_parts(pcm.as_ptr().cast::<u8>(), pcm.len() * 2) };

    let mut out = fs::File::create(dest)?;
    out.write_all(&header)?;
    out.write_all(samples)?;
    out.flush()?;
    Ok(())
}

/// Find `name` in `dir`, tolerating a mismatched case. Scene sheets are often
/// written against an ISO9660 listing (upper case) while the files ship lower
/// case — harmless on Windows, but the core can't open them on a case-sensitive
/// filesystem.
fn resolve_ignoring_case(dir: &Path, name: &str) -> Option<PathBuf> {
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name))
        })
}

/// Link `src` into `dest`, falling back to a copy across filesystems. Data
/// tracks run to hundreds of megabytes, so a hard link is worth trying first.
fn link_or_copy(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest)?;
    Ok(())
}

/// No libretro PSX core decodes MP3 audio tracks — they read the compressed
/// bytes straight through as PCM, which comes out as full-scale noise. If a cue
/// references any, build a parallel disc directory in the cache with those
/// tracks decoded to WAV and the sheet rewritten to match. Data tracks are hard
/// linked, so the copy is nearly free.
///
/// Returns the rewritten cue, or `None` if every track is already playable.
pub fn prepare_psx_disc(cue_path: &Path) -> Result<Option<PathBuf>> {
    let text = fs::read_to_string(cue_path)?;
    let files = parse_cue_files(&text);
    if files.is_empty() {
        return Ok(None);
    }
    let dir = cue_path.parent().unwrap_or(Path::new("."));

    // Resolve every referenced name first: the sheet's spelling may differ from
    // the file's, and the rewrite has to use what's actually on disk.
    let mut resolved = Vec::new();
    for f in &files {
        let Some(src) = resolve_ignoring_case(dir, &f.name) else {
            bail!("cue references missing file {:?}", dir.join(&f.name));
        };
        let actual = src
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        resolved.push((f, src, actual));
    }

    let has_mp3 = files.iter().any(|f| f.kind.eq_ignore_ascii_case("MP3"));
    let miscased = resolved.iter().any(|(f, _, actual)| f.name != *actual);
    if !has_mp3 && !miscased {
        return Ok(None);
    }
    if miscased {
        debug!("Cue file names don't match on-disk case; rewriting {cue_path:?}");
    }

    // Key the cache on the disc's *contents* — never its path or mtime. A disc
    // unpacked from a zip lands in a fresh temp dir each run and the extractor
    // doesn't restore the archived timestamps, so either would miss the cache
    // every launch and pile up another copy of the transcoded audio.
    let mut key = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    text.hash(&mut key);
    for (f, src, actual) in &resolved {
        actual.hash(&mut key);
        if let Ok(meta) = fs::metadata(src) {
            meta.len().hash(&mut key);
        }
        // Size alone would let an edited track reuse stale audio. Hash the bytes
        // of the tracks we actually decode; they're a few MB, unlike the data
        // track, which can be most of a gigabyte.
        if f.kind.eq_ignore_ascii_case("MP3")
            && let Ok(bytes) = fs::read(src)
        {
            bytes.hash(&mut key);
        }
    }
    let stem = cue_path.file_stem().unwrap_or_default().to_string_lossy();
    let out_dir = dirs::cache_dir()
        .unwrap_or_default()
        .join("demarc")
        .join("cdda")
        .join(format!("{stem}-{:016x}", key.finish()));
    let out_cue = out_dir.join("disc.cue");
    if out_cue.is_file() {
        debug!("Using cached disc {out_dir:?}");
        return Ok(Some(out_cue));
    }
    fs::create_dir_all(&out_dir)?;

    let mut new_text = text.clone();
    for (f, src, actual) in &resolved {
        if f.kind.eq_ignore_ascii_case("MP3") {
            let wav_name = format!(
                "{}.wav",
                Path::new(actual)
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            info!("Transcoding CD audio track {actual:?} -> {wav_name}");
            transcode_mp3_to_wav(src, &out_dir.join(&wav_name))?;
            new_text = new_text.replace(f.line, &format!("FILE \"{wav_name}\" WAVE"));
        } else {
            link_or_copy(src, &out_dir.join(actual))?;
            // Quote the name so bare names parse, and use the on-disk spelling.
            new_text = new_text.replace(f.line, &format!("FILE \"{actual}\" {}", f.kind));
        }
    }
    fs::write(&out_cue, new_text)?;
    Ok(Some(out_cue))
}

/// True if `path` is a raw PlayStation executable rather than a disc image.
/// These need a different core from a `.cue`/`.chd` — see `get_core`.
pub fn is_psx_exe(path: &Path) -> bool {
    read_header(path, 8).is_ok_and(|h| h == b"PS-X EXE")
}

/// The fixed size of a PSX executable's header. Everything after it is the
/// text section.
const PSX_HEADER_LEN: u64 = 0x800;

/// Offset of the text section's size (`t_size`) in that header, right after the
/// address it loads to (`t_addr`) at `0x18`.
const PSX_TEXT_SIZE_OFFSET: usize = 0x1c;

/// The PlayStation's 2MB of main RAM, which the text section has to fit into.
const PSX_RAM_SIZE: u32 = 0x20_0000;

/// The text size the PSX executable at `path` should be recording, or `None`
/// when it isn't one, or already records exactly that.
///
/// The core takes only one number: `t_size` has to equal the data that follows
/// the header, byte for byte, or it refuses to load the file at all ("Text
/// section recorded size is smaller/larger than data available in file"). Scene
/// releases keep running into the small side of that — the demo's data was
/// appended after the code without the header being updated to match — so the
/// fix is to record the size the file really has.
///
/// What the section can't do is run off the end of RAM. The copy is mirrored
/// the way the hardware maps it, so the overflow lands back at address zero, on
/// top of the kernel, and the demo dies there. A file holding more than fits
/// from its load address is cut down to what does by [`fix_psx_text_size`],
/// which is also what keeps this within the 2MB the core will accept at all.
fn psx_text_size_fix(path: &Path) -> Option<u32> {
    let header = read_header(path, PSX_TEXT_SIZE_OFFSET + 4).ok()?;
    if header.len() < PSX_TEXT_SIZE_OFFSET + 4 || header[0..8] != *b"PS-X EXE" {
        return None;
    }
    let field = |off: usize| u32::from_le_bytes(header[off..off + 4].try_into().unwrap());

    let available = fs::metadata(path)
        .ok()?
        .len()
        .saturating_sub(PSX_HEADER_LEN);
    // RAM is mirrored, so only the load address' offset within it says how much
    // room the text section has before it runs off the end.
    let room = PSX_RAM_SIZE - (field(0x18) & (PSX_RAM_SIZE - 1));
    let size = u32::try_from(available).unwrap_or(u32::MAX).min(room);

    // A file with nothing after its header is broken in a way no size helps
    // with; leave it to fail with the core's own complaint.
    (size > 0 && size != field(PSX_TEXT_SIZE_OFFSET)).then_some(size)
}

/// True if [`fix_psx_text_size`] would rewrite `path`.
pub fn psx_needs_text_fix(path: &Path) -> bool {
    psx_text_size_fix(path).is_some()
}

/// Rewrite the text section size of the PSX executable at `path` to the size
/// the core demands — see [`psx_text_size_fix`] — cutting the file down to that
/// size when it holds more than fits in RAM, since the core insists the two
/// agree. Returns whether the file was changed.
pub fn fix_psx_text_size(path: &Path) -> Result<bool> {
    use std::io::{Seek, SeekFrom, Write};

    let Some(size) = psx_text_size_fix(path) else {
        return Ok(false);
    };
    let mut file = fs::OpenOptions::new().write(true).open(path)?;
    file.seek(SeekFrom::Start(PSX_TEXT_SIZE_OFFSET as u64))?;
    file.write_all(&size.to_le_bytes())?;
    file.set_len(PSX_HEADER_LEN + u64::from(size))?;
    Ok(true)
}

/// How much of a file [`is_gba_rom`] needs to see: the whole 0xc0-byte
/// cartridge header, up to but not including the entry point it branches to.
pub const GBA_HEADER_LEN: usize = 0xc0;

/// True if `header` — the start of a file, at least [`GBA_HEADER_LEN`] bytes of
/// it — looks like a Game Boy Advance cartridge.
///
/// A GBA ROM opens with an unconditional ARM branch past the header, followed
/// by the Nintendo logo the BIOS checks on boot and a fixed `0x96` at 0xb2.
/// The logo is what makes this reliable — only its first bytes are compared,
/// since a ROM that got that far is never anything else.
///
/// Scene releases meant for a flash cart or an emulator often ship with the
/// logo blanked out (it is Nintendo's artwork, and only the real BIOS cares),
/// so a ROM without it still counts if the rest of the header holds together:
/// the reserved fields are zero and the complement check over 0xa0..=0xbc is
/// correct. That checksum is computed over the bytes right before it, so
/// hitting it by accident takes the same 1-in-256 luck as each of the fixed
/// bytes on top of it.
pub fn is_gba_rom(header: &[u8]) -> bool {
    /// Start of the 156-byte Nintendo logo at offset 0x04.
    const LOGO: [u8; 8] = [0x24, 0xff, 0xae, 0x51, 0x69, 0x9a, 0xa2, 0x21];

    if header.len() < GBA_HEADER_LEN || header[3] != 0xea || header[0xb2] != 0x96 {
        return false;
    }
    if header[0x04..0x04 + LOGO.len()] == LOGO {
        return true;
    }

    // `b` at offset 0, with the 24-bit signed word offset the ARM pipeline
    // measures from 0x08. The entry point has to land past the header.
    let offset = i32::from_le_bytes([header[0], header[1], header[2], 0]) << 8 >> 8;
    let entry = 8 + i64::from(offset) * 4;
    // 0xb3 main unit code and 0xb4 device type are 0 on everything but
    // Nintendo's own debug hardware; 0xb5..=0xbb and 0xbe..=0xbf are reserved.
    let reserved_zero =
        header[0xb3..0xbc].iter().all(|&b| b == 0) && header[0xbe] == 0 && header[0xbf] == 0;
    let sum = header[0xa0..=0xbc]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    let complement = 0u8.wrapping_sub(sum.wrapping_add(0x19));

    (0xc0..=0x0200_0000).contains(&entry) && reserved_zero && header[0xbd] == complement
}

/// Read up to `len` bytes from the start of `path`. Returns fewer bytes if the
/// file is shorter.
pub fn read_header(path: &Path, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = vec![0u8; len];
    let mut file = fs::File::open(path)?;
    let mut got = 0;
    while got < len {
        match file.read(&mut buf[got..])? {
            0 => break,
            n => got += n,
        }
    }
    buf.truncate(got);
    Ok(buf)
}

/// True if a `.cue` sheet declares at least one non-audio track. Game discs
/// carry their data in a `MODE1`/`MODE2` track; a pure audio-CD rip has only
/// `TRACK nn AUDIO` entries.
pub fn cue_has_data_track(path: &Path) -> bool {
    let Ok(head) = read_header(path, 64 * 1024) else {
        return false;
    };
    String::from_utf8_lossy(&head).lines().any(|line| {
        let line = line.trim();
        line.starts_with("TRACK") && !line.ends_with("AUDIO")
    })
}

pub struct M3u {
    pub tags: HashMap<String, String>,
    pub files: Vec<PathBuf>,
}

pub fn parse_m3u(path: &Path) -> Result<M3u> {
    let contents = std::fs::read_to_string(path)?;
    let mut tags = HashMap::new();
    let mut files: Vec<PathBuf> = vec![];
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let mut remaining = rest;
            while let Some(eq) = remaining.find("=\"") {
                let key_start = remaining[..eq]
                    .rfind(|c: char| c.is_whitespace() || c == ',')
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let key = remaining[key_start..eq].trim();
                let after_quote = &remaining[eq + 2..];
                let Some(end) = after_quote.find('"') else {
                    break;
                };
                let value = &after_quote[..end];
                if !key.is_empty() {
                    tags.insert(key.to_string(), value.to_string());
                }
                remaining = &after_quote[end + 1..];
            }
        } else if !line.starts_with('#') {
            files.push(line.into());
        }
    }
    Ok(M3u { tags, files })
}

pub fn has_matching(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.to_lowercase().contains(&name.to_lowercase()));
        matches.then_some(path)
    })
}

fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|e| {
        let path = e.path();
        let matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(name));
        matches.then_some(path)
    })
}

/// True if `game` is a directory containing an `s/startup-sequence` boot script,
pub fn is_self_booting_dir(game: &Path) -> bool {
    find_child(game, "s").is_some_and(|s_dir| find_child(&s_dir, "startup-sequence").is_some())
}
/// Build a bootable Atari ST FAT12 floppy image containing an `AUTO` directory
/// with `data` (a GEMDOS executable from `src`) copied into it, so it runs
/// automatically when the disk boots. Returns the path to the `.st` image and
/// the fresh temp directory it lives in, which the caller has to keep alive for
/// as long as the image is needed.
pub fn build_atari_auto_disk(data: &[u8]) -> Result<(PathBuf, TempDir)> {
    use std::io::Write;

    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
    let img_path = target_dir.path().join("disk.st");

    let img = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&img_path)?;
    img.set_len(720 * 1024)?;
    fatfs::format_volume(
        &img,
        fatfs::FormatVolumeOptions::new()
            .fat_type(fatfs::FatType::Fat12)
            .bytes_per_sector(512)
            .total_sectors(1440) // 720K = 1440 * 512
            .bytes_per_cluster(1024) // 2 sectors per cluster
            .max_root_dir_entries(112)
            .fats(2)
            .media(0xF9)
            .sectors_per_track(9)
            .heads(2)
            .volume_id(rand::random()),
    )?;

    let prog_name = "STARTME.PRG";

    let fs = fatfs::FileSystem::new(&img, fatfs::FsOptions::new())?;
    {
        let auto = fs.root_dir().create_dir("AUTO")?;
        let mut prog = auto.create_file(prog_name)?;
        prog.write_all(data)?;
        prog.flush()?;
    }
    fs.unmount()?;

    Ok((img_path, target_dir))
}

/// Copy `files` into a fresh temp directory and write a `demo.m3u` that
/// references each copied file by name. Returns the path to the `.m3u`,
/// alongside the copies in the temp directory, and the directory itself — the
/// caller has to keep it alive for as long as the playlist is needed. Since the
/// files are copied, whatever they came from can go away right after.
pub fn build_m3u(files: &[PathBuf]) -> Result<(PathBuf, TempDir)> {
    use std::io::Write;

    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;

    let mut contents = String::from("#EXTM3U\n");
    for file in files {
        let name = file
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("invalid file path: {:?}", file))?;
        fs::copy(file, target_dir.path().join(name))?;
        contents.push_str(&name.to_string_lossy());
        contents.push('\n');
    }

    let m3u_path = target_dir.path().join("demo.m3u");
    let mut m3u = fs::File::create(&m3u_path)?;
    m3u.write_all(contents.as_bytes())?;
    m3u.flush()?;

    Ok((m3u_path, target_dir))
}

/// Sort disk images so that the most "main" disk comes first. Ordering rules:
/// 1. Files whose stem ends in a digit (a digit right next to the extension dot)
///    come first, e.g. `disk3.d64`.
/// 2. Files that contain a digit somewhere else come next, e.g. `disk2_extra.d64`.
/// 3. Files with no digit at all come last, e.g. `anything.d64`.
///
/// Within each group files are ordered by name for a stable, predictable result.
pub fn sort_disks(files: &mut [PathBuf]) {
    fn rank(path: &Path) -> u8 {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if stem.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            0
        } else if stem.chars().any(|c| c.is_ascii_digit()) {
            1
        } else {
            2
        }
    }

    files.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| a.file_name().cmp(&b.file_name()))
    });
}

/// Archive formats [`unpack_to_temp`] knows how to extract.
fn is_supported_archive(format: ArchiveFormat) -> bool {
    matches!(
        format,
        ArchiveFormat::Zip
            | ArchiveFormat::SevenZ
            | ArchiveFormat::Rar
            | ArchiveFormat::Lha
            | ArchiveFormat::Tar
    ) || is_single_file_compressor(format)
}

/// Formats that compress one unnamed payload rather than holding a set of named
/// files, so the payload's name has to come from the archive's own (see
/// [`unpack_into`]) and the whole thing can be unpacked straight to bytes (see
/// [`unpack_if_packed`]).
fn is_single_file_compressor(format: ArchiveFormat) -> bool {
    matches!(
        format,
        ArchiveFormat::Z | ArchiveFormat::Gz | ArchiveFormat::Bz2
    )
}

/// Decompress `data` when it is a gzip, bzip2 or Unix-compress stream, and
/// return it unchanged otherwise. For data files that are packed on their own
/// instead of bundled in an archive — a gzipped db — where the point is the
/// bytes, not files on disk as with [`unpack_into`].
pub fn unpack_if_packed(data: Vec<u8>) -> Result<Vec<u8>> {
    let Some(format) = ArchiveFormat::detect_from_bytes(&data) else {
        return Ok(data);
    };
    if !is_single_file_compressor(format) {
        return Ok(data);
    }
    let mut archive = format.open(std::io::Cursor::new(&data[..]))?;
    let Some(entry) = archive.next_entry()? else {
        bail!("{} stream holds nothing", format.name());
    };
    Ok(archive.read(&entry)?)
}

/// If `path` is one of the supported archive formats — zip, 7z, rar, lha/lzh,
/// tar, gz, bz2 or Unix compress (`.Z`) — extract it into a fresh temp
/// directory and return that directory. The format is detected from the file
/// contents (falling back to the extension), so mis-named archives still work.
/// Returns `Ok(None)` when `path` is not a recognised archive — dropping the
/// [`TempDir`] then takes the empty directory with it, and most files handed to
/// this are not archives at all.
pub fn unpack_to_temp(path: &Path) -> Result<Option<TempDir>> {
    let target_dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
    match unpack_into(path, target_dir.path()) {
        Ok(true) => Ok(Some(target_dir)),
        other => other.map(|_| None),
    }
}

/// Extract the archive at `path` into the existing directory `target_dir`,
/// which is written into directly — see [`unpack_to_temp`] for the variant that
/// makes a temp directory of its own. Returns `false`, having written nothing,
/// when `path` is not a recognised archive.
pub fn unpack_into(path: &Path, target_dir: &Path) -> Result<bool> {
    use std::{io::BufReader, path::Component};

    let mut file = BufReader::new(fs::File::open(path)?);
    let Some(format) = ArchiveFormat::detect(&mut file, Some(path))? else {
        return Ok(false);
    };
    if !is_supported_archive(format) {
        return Ok(false);
    }

    let mut archive = format.open(file)?;
    // Single-file compressors (.Z/.gz/.bz2) carry no name for their payload, so
    // derive one from the archive's stem (e.g. `demo.tar.gz` -> `demo.tar`).
    if is_single_file_compressor(format)
        && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
    {
        archive.set_single_file_name(stem.to_string());
    }

    while let Some(entry) = archive.next_entry()? {
        let name = entry.name();
        // Keep only normal path components so an absolute path or `..` in the
        // archive can't write outside the target directory.
        let rel: PathBuf = Path::new(name)
            .components()
            .filter(|c| matches!(c, Component::Normal(_)))
            .collect();
        if rel.as_os_str().is_empty() {
            // Unusable name (e.g. all `..`): nothing safe to write.
            archive.skip(&entry)?;
            continue;
        }
        let out_path = target_dir.join(&rel);
        if name.ends_with('/') || name.ends_with('\\') {
            // Explicit directory entry (zip, tar).
            fs::create_dir_all(&out_path)?;
            archive.skip(&entry)?;
            continue;
        }
        // Some formats (e.g. rar) mark directories only in per-file metadata the
        // unified reader doesn't expose, but always decompress them to nothing.
        // Treating an empty entry as a directory both handles those and keeps a
        // zero-length file from blocking a later `dir/child` from being created.
        let data = archive.read(&entry)?;
        if data.is_empty() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out_path, &data)?;
        }
    }
    Ok(true)
}

pub fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> std::io::Result<()> {
    fs::create_dir_all(&dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

/// Result of recursively scanning a release directory.
pub struct ScannedDir {
    /// Disk images (`.d64`, `.adf`, `.atr`) found anywhere under the directory.
    pub disk_images: Vec<PathBuf>,
    /// The first recognized file of any type encountered during the walk.
    pub first_file: Option<PathBuf>,
    /// System type of the last disk image found, or of the first recognized
    /// file when no disk images were present. `Unknown` if nothing matched.
    pub system_type: SystemType,
}

/// Recursively scan `dir`, collecting disk images and remembering the first
/// recognized file along with the system type that should be used.
pub fn scan_release_dir(dir: &Path) -> Result<ScannedDir> {
    let mut disk_images = vec![];
    let mut first_file = None;
    let mut system_type = SystemType::Unknown;

    for entry in fs::read_dir(dir)? {
        let path = entry?.path();

        if path.is_dir() {
            let sub = scan_release_dir(&path)?;
            if first_file.is_none() {
                first_file = sub.first_file;
            }
            if sub.system_type != SystemType::Unknown {
                system_type = sub.system_type;
            }
            disk_images.extend(sub.disk_images);
            continue;
        }

        let t = get_system_type(&path);
        if t == SystemType::Unknown {
            continue;
        }
        // Avoid showing screenshots instead of playing actual demo
        if first_file.is_none() || system_type == SystemType::Gfx {
            first_file = Some(path.clone());
            system_type = t;
        }
        if is_disk_image(&path) {
            disk_images.push(path);
            system_type = t;
        }
    }

    Ok(ScannedDir {
        disk_images,
        first_file,
        system_type,
    })
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A logo-less header like the scene releases carry: branch to 0xc0, the
    /// fixed 0x96, and a maker code the complement check accounts for.
    fn logoless_gba_header() -> Vec<u8> {
        let mut h = vec![0u8; GBA_HEADER_LEN];
        h[0..4].copy_from_slice(&[0x2e, 0x00, 0x00, 0xea]);
        h[0xb0] = b'0';
        h[0xb1] = b'1';
        h[0xb2] = 0x96;
        h[0xbd] = 0xf0;
        h
    }

    /// The Nintendo logo is Nintendo's, so cracktros and flash-cart builds
    /// routinely blank it. The rest of the header still has to add up.
    #[test]
    fn gba_rom_detected_without_logo() {
        let header = logoless_gba_header();
        assert!(is_gba_rom(&header));

        // Everything the logo-less path leans on, broken one field at a time.
        for (offset, value) in [
            (0x03, 0xeb), // conditional branch, not `b`
            (0xb2, 0x00), // fixed byte
            (0xb5, 0x01), // reserved
            (0xbd, 0xf1), // complement check
            (0x00, 0x00), // entry point inside the header
        ] {
            let mut broken = header.clone();
            broken[offset] = value;
            assert!(
                !is_gba_rom(&broken),
                "accepted with {offset:#x} = {value:#x}"
            );
        }
    }

    /// A real ROM keeps its logo, and a truncated read is never a match.
    #[test]
    fn gba_rom_detected_with_logo() {
        let mut header = vec![0u8; GBA_HEADER_LEN];
        header[0..4].copy_from_slice(&[0x2e, 0x00, 0x00, 0xea]);
        header[0x04..0x0c].copy_from_slice(&[0x24, 0xff, 0xae, 0x51, 0x69, 0x9a, 0xa2, 0x21]);
        header[0xb2] = 0x96;
        // Bad complement check and non-zero reserved fields don't matter here.
        header[0xb5] = 0x42;
        assert!(is_gba_rom(&header));
        assert!(!is_gba_rom(&header[..GBA_HEADER_LEN - 1]));
    }

    /// Scene sheets quote the file name inconsistently, and the track kind is
    /// what decides whether a track needs transcoding.
    #[test]
    fn cue_file_lines_parse_quoted_and_bare() {
        let files = parse_cue_files(
            "FILE mono_t1.bin BINARY\n  TRACK 01 MODE2/2352\n\
             FILE \"my track.mp3\" MP3\n  TRACK 02 AUDIO\n\
             FILE \"Pawlov.bin\" BINARY\n",
        );
        let got: Vec<_> = files.iter().map(|f| (&*f.name, &*f.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("mono_t1.bin", "BINARY"),
                ("my track.mp3", "MP3"),
                ("Pawlov.bin", "BINARY"),
            ]
        );
    }

    /// The WAV must be exactly CD audio, since the core skips the header and
    /// reads the remainder as raw PCM.
    #[test]
    fn wav_header_is_cd_audio() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("a.wav");
        write_wav(&path, &[0, 0, 1, -1]).unwrap();
        let d = fs::read(&path).unwrap();
        assert_eq!(&d[0..4], b"RIFF");
        assert_eq!(&d[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([d[22], d[23]]), 2, "channels");
        assert_eq!(
            u32::from_le_bytes([d[24], d[25], d[26], d[27]]),
            CDDA_RATE,
            "sample rate"
        );
        assert_eq!(u16::from_le_bytes([d[34], d[35]]), 16, "bit depth");
        assert_eq!(d.len(), 44 + 8, "header + 4 samples");
    }

    /// A cue with no compressed tracks and matching case must be left exactly
    /// as-is — no cache directory, no rewrite.
    #[test]
    fn cue_without_mp3_is_not_rewritten() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        fs::write(dir.path().join("d.bin"), [0u8; 16]).unwrap();
        let cue = dir.path().join("plain.cue");
        fs::write(&cue, "FILE \"d.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        assert!(prepare_psx_disc(&cue).unwrap().is_none());
    }

    /// Scene sheets are often written in ISO9660 upper case while the files ship
    /// lower case. That only breaks on a case-sensitive filesystem, so the disc
    /// gets rewritten to the names actually on disk.
    #[test]
    fn miscased_cue_names_are_resolved() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        fs::write(dir.path().join("disc_t1.bin"), [0u8; 16]).unwrap();
        let cue = dir.path().join("game.cue");
        fs::write(&cue, "FILE DISC_T1.BIN BINARY\n  TRACK 01 MODE2/2352\n").unwrap();

        let out = prepare_psx_disc(&cue)
            .unwrap()
            .expect("should be rewritten");
        let text = fs::read_to_string(&out).unwrap();
        assert!(
            text.contains("\"disc_t1.bin\""),
            "cue should use the on-disk spelling, got: {text}"
        );
        assert!(out.parent().unwrap().join("disc_t1.bin").is_file());
        let _ = fs::remove_dir_all(out.parent().unwrap());
    }

    /// The cache is keyed on disc contents, not location. A disc unpacked from a
    /// zip gets a fresh temp dir every run, and keying on the path (or on mtime,
    /// which extraction doesn't restore) would re-transcode it each launch.
    #[test]
    fn disc_cache_key_ignores_path_and_mtime() {
        let mut seen = vec![];
        for _ in 0..2 {
            let dir = tempfile::Builder::new()
                .prefix("demarc-")
                .tempdir()
                .unwrap();
            fs::write(dir.path().join("t1.bin"), [7u8; 32]).unwrap();
            let cue = dir.path().join("same.cue");
            fs::write(&cue, "FILE T1.BIN BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
            seen.push(prepare_psx_disc(&cue).unwrap().unwrap());
        }
        assert_eq!(
            seen[0], seen[1],
            "identical discs must share one cache entry"
        );
        let _ = fs::remove_dir_all(seen[0].parent().unwrap());
    }

    /// A game disc's cue sheet is PlayStation; an audio-CD rip of the same
    /// shape is not, so a music library doesn't get treated as a game.
    #[test]
    fn cue_data_track_distinguishes_discs_from_audio_rips() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();

        let game = dir.path().join("game.cue");
        fs::write(
            &game,
            "FILE \"game.bin\" BINARY\n  TRACK 01 MODE2/2352\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        assert_eq!(get_system_type(&game), SystemType::Psx);

        let album = dir.path().join("album.cue");
        fs::write(
            &album,
            "REM GENRE Electronic\nFILE \"01.wav\" WAVE\n  TRACK 01 AUDIO\n    INDEX 01 00:00:00\n",
        )
        .unwrap();
        assert_eq!(get_system_type(&album), SystemType::Unknown);
    }

    /// Mixed-mode discs lead with a data track and follow it with CD audio.
    #[test]
    fn mixed_mode_cue_is_psx() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let cue = dir.path().join("mixed.cue");
        fs::write(
            &cue,
            "FILE \"d.bin\" BINARY\n  TRACK 01 MODE1/2352\n    INDEX 01 00:00:00\n  TRACK 02 AUDIO\n    INDEX 01 05:00:00\n",
        )
        .unwrap();
        assert_eq!(get_system_type(&cue), SystemType::Psx);
    }

    #[test]
    fn psx_exe_is_detected_by_magic() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let exe = dir.path().join("demo.exe");
        let mut data = b"PS-X EXE".to_vec();
        data.resize(2048, 0);
        fs::write(&exe, &data).unwrap();
        assert_eq!(get_system_type(&exe), SystemType::Psx);
        // Both halves matter: the type picks the system, `is_psx_exe` picks the
        // core, because pcsx_rearmed can't load a raw executable.
        assert!(is_psx_exe(&exe));

        let disc = dir.path().join("disc.cue");
        fs::write(&disc, "FILE \"d.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        assert!(!is_psx_exe(&disc));
    }

    /// Build a PSX executable loading to `t_addr`, recording `t_size`, and
    /// holding `data_len` bytes after its 0x800 byte header.
    fn write_psx_exe(path: &Path, t_addr: u32, t_size: u32, data_len: usize) {
        let mut data = b"PS-X EXE".to_vec();
        data.resize(0x18, 0);
        data.extend_from_slice(&t_addr.to_le_bytes());
        data.extend_from_slice(&t_size.to_le_bytes());
        data.resize(0x800 + data_len, 0);
        fs::write(path, &data).unwrap();
    }

    fn text_size(path: &Path) -> u32 {
        let header = read_header(path, 0x20).unwrap();
        u32::from_le_bytes(header[0x1c..0x20].try_into().unwrap())
    }

    /// A header that undercounts the file is rewritten to the whole of it, and
    /// only that field changes.
    #[test]
    fn short_psx_text_size_is_patched() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let exe = dir.path().join("demo.psx");
        write_psx_exe(&exe, 0x8001_0000, 0x800, 0x4000);

        assert!(psx_needs_text_fix(&exe));
        assert!(fix_psx_text_size(&exe).unwrap());
        assert_eq!(text_size(&exe), 0x4000);
        assert_eq!(fs::metadata(&exe).unwrap().len(), 0x800 + 0x4000);

        // Nothing left to do the second time around.
        assert!(!psx_needs_text_fix(&exe));
        assert!(!fix_psx_text_size(&exe).unwrap());
    }

    /// A header counting more than the file holds — its own 0x800 bytes, say —
    /// is just as unloadable as one counting less, so it is brought down to
    /// what's there. Only a size that already matches is left alone.
    #[test]
    fn psx_text_size_is_matched_to_the_file() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();

        let exact = dir.path().join("exact.psx");
        write_psx_exe(&exact, 0x8001_0000, 0x4000, 0x4000);
        assert!(!psx_needs_text_fix(&exact));

        let over = dir.path().join("over.psx");
        write_psx_exe(&over, 0x8001_0000, 0x4800, 0x4000);
        assert!(fix_psx_text_size(&over).unwrap());
        assert_eq!(text_size(&over), 0x4000);

        let not_an_exe = dir.path().join("plain.bin");
        fs::write(&not_an_exe, [0u8; 0x1000]).unwrap();
        assert!(!psx_needs_text_fix(&not_an_exe));
    }

    /// The text section can't be grown past the end of the 2MB of main RAM it
    /// loads into, however much data the file holds — what doesn't fit would
    /// wrap onto the kernel at address zero. The file is cut down with it, since
    /// the core won't load a size that disagrees with what's there.
    #[test]
    fn psx_text_size_stops_at_end_of_ram() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();

        // Loading 0x10000 below the top of RAM leaves room for that much only,
        // even though the file has twice as much in it.
        let exe = dir.path().join("high.psx");
        write_psx_exe(&exe, 0x801f_0000, 0x800, 0x20000);
        assert!(fix_psx_text_size(&exe).unwrap());
        assert_eq!(text_size(&exe), 0x10000);
        assert_eq!(fs::metadata(&exe).unwrap().len(), 0x800 + 0x10000);

        // And what's left agrees with the header, so it loads as it stands.
        assert!(!psx_needs_text_fix(&exe));
    }
}
