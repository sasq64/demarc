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

/// One sector of an ISO9660 image: the user-data half of a CD-ROM data sector,
/// and the unit every address in the format counts in.
const ISO_SECTOR: usize = 2048;

/// Where each piece of a [`build_psx_iso`] image lives. The first 16 sectors are
/// the system area — a pressed disc keeps Sony's licence data there, but nothing
/// reads it here, since the core boots with an HLE BIOS — and the rest is the
/// smallest filesystem that still describes two files.
const LBA_PVD: u32 = 16;
const LBA_TERMINATOR: u32 = 17;
const LBA_PATH_TABLE_L: u32 = 18;
const LBA_PATH_TABLE_M: u32 = 19;
const LBA_ROOT_DIR: u32 = 20;
const LBA_SYSTEM_CNF: u32 = 21;
const LBA_EXE: u32 = 22;

/// The name the executable is given on the disc. Both the BIOS and every core
/// fall back to this one when a disc has no `SYSTEM.CNF`, so it is also what
/// makes the image boot if the sheet is ever ignored.
const ISO_EXE_NAME: &[u8] = b"PSX.EXE;1";

/// An ISO9660 32-bit field: the value little endian, then big endian again.
/// Sizes and addresses are all stored twice so a reader of either byte order can
/// take the half it likes.
fn both_endian32(value: u32) -> [u8; 8] {
    let mut out = [0u8; 8];
    out[0..4].copy_from_slice(&value.to_le_bytes());
    out[4..8].copy_from_slice(&value.to_be_bytes());
    out
}

/// [`both_endian32`] for the format's 16-bit fields.
fn both_endian16(value: u16) -> [u8; 4] {
    let mut out = [0u8; 4];
    out[0..2].copy_from_slice(&value.to_le_bytes());
    out[2..4].copy_from_slice(&value.to_be_bytes());
    out
}

/// Write `text` into `buf` at `at` as a fixed-width, space-padded field, which
/// is how ISO9660 stores every string.
fn put_padded(buf: &mut [u8], at: usize, len: usize, text: &str) {
    buf[at..at + len].fill(b' ');
    let n = text.len().min(len);
    buf[at..at + n].copy_from_slice(&text.as_bytes()[..n]);
}

/// An ISO9660 directory record naming `len` bytes at `lba`. `name` is the
/// identifier exactly as it goes on the disc, so files keep the `;1` version
/// suffix and the `.`/`..` entries are the single bytes 0 and 1.
///
/// Records are padded to an even length, which is what makes the one-byte-named
/// entries — and so the copy of the root's record inside the volume descriptor —
/// 34 bytes.
fn dir_record(name: &[u8], lba: u32, len: u32, is_dir: bool) -> Vec<u8> {
    let mut rec = vec![0u8; 33 + name.len()];
    rec[2..10].copy_from_slice(&both_endian32(lba));
    rec[10..18].copy_from_slice(&both_endian32(len));
    // Recording time, as years-since-1900 down to seconds plus a timezone in
    // quarter hours. Nothing on the PlayStation side reads it; a fixed
    // 1980-01-01 GMT keeps the image byte-identical between builds, which the
    // content-keyed cache below relies on.
    rec[18..25].copy_from_slice(&[80, 1, 1, 0, 0, 0, 0]);
    rec[25] = if is_dir { 0x02 } else { 0x00 };
    rec[28..32].copy_from_slice(&both_endian16(1)); // volume sequence number
    rec[32] = name.len() as u8;
    rec[33..].copy_from_slice(name);
    if !rec.len().is_multiple_of(2) {
        rec.push(0);
    }
    rec[0] = rec.len() as u8;
    rec
}

/// The primary volume descriptor: the sector at a fixed 16 that every reader
/// starts from, and which points at the root directory's own record.
fn primary_volume_descriptor(total_sectors: u32, path_table_size: u32, root: &[u8]) -> Vec<u8> {
    let mut pvd = vec![0u8; ISO_SECTOR];
    pvd[0] = 1; // primary volume descriptor
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1; // descriptor version
    // A pressed PlayStation disc says exactly this, and some tools identify one
    // by it. Nothing refuses to boot without it, but it costs 11 bytes.
    put_padded(&mut pvd, 8, 32, "PLAYSTATION");
    put_padded(&mut pvd, 40, 32, "DEMARC");
    pvd[80..88].copy_from_slice(&both_endian32(total_sectors));
    pvd[120..124].copy_from_slice(&both_endian16(1)); // volume set size
    pvd[124..128].copy_from_slice(&both_endian16(1)); // volume sequence number
    pvd[128..132].copy_from_slice(&both_endian16(ISO_SECTOR as u16));
    pvd[132..140].copy_from_slice(&both_endian32(path_table_size));
    // The two path tables are the one pair of fields stored as a plain value in
    // each byte order rather than both-endian.
    pvd[140..144].copy_from_slice(&LBA_PATH_TABLE_L.to_le_bytes());
    pvd[148..152].copy_from_slice(&LBA_PATH_TABLE_M.to_be_bytes());
    pvd[156..156 + root.len()].copy_from_slice(root);
    for (at, len) in [(190, 128), (318, 128), (446, 128), (574, 128)] {
        put_padded(&mut pvd, at, len, "");
    }
    for at in [702, 739, 776] {
        put_padded(&mut pvd, at, 37, "");
    }
    // Creation and modification, then expiration and effective, as
    // YYYYMMDDHHMMSSCC plus a timezone byte. All zeros means "unspecified",
    // which is what a disc that never expires records.
    for at in [813, 830] {
        pvd[at..at + 17].copy_from_slice(b"1980010100000000\0");
    }
    for at in [847, 864] {
        pvd[at..at + 17].copy_from_slice(b"0000000000000000\0");
    }
    pvd[881] = 1; // file structure version
    pvd
}

/// Lay `exe` out as a bootable PlayStation disc image: a `SYSTEM.CNF` naming the
/// boot file and the executable itself, in a filesystem with nothing else in it.
///
/// The console's boot path is what decides the shape here. It reads the volume
/// descriptor at sector 16, follows the root directory record inside it, looks
/// for `SYSTEM.CNF`, takes the `BOOT = cdrom:\…` line from it, and loads that
/// file as a PS-X EXE — the file's first sector being the executable's own
/// 0x800-byte header, which is why the executable goes on the disc unaltered.
/// Cores that HLE the BIOS (pcsx_rearmed) walk the same structures themselves.
fn build_psx_iso(exe: &[u8]) -> Vec<u8> {
    const CNF_NAME: &[u8] = b"SYSTEM.CNF;1";
    // `BOOT` is the only line a core reads; the rest is what a real disc carries
    // and what the console's own BIOS would set up from.
    const CNF: &str = "BOOT = cdrom:\\PSX.EXE;1\r\nTCB = 4\r\nEVENT = 10\r\nSTACK = 801FFFF0\r\n";

    let exe_sectors = exe.len().div_ceil(ISO_SECTOR) as u32;
    let mut total_sectors = LBA_EXE + exe_sectors;
    // pcsx_rearmed tells a 2048-byte-sector image from a raw 2352-byte one by
    // the file's length alone, so a length that divides evenly by both would be
    // read as the wrong kind of disc — which only happens at a multiple of 147
    // sectors. An extra empty sector steps past it.
    if total_sectors.is_multiple_of(147) {
        total_sectors += 1;
    }

    // The root directory: itself, its parent (itself again, since it is the
    // root), and the two files. One sector holds all four with room to spare.
    let mut root_dir = Vec::with_capacity(ISO_SECTOR);
    for rec in [
        dir_record(&[0], LBA_ROOT_DIR, ISO_SECTOR as u32, true),
        dir_record(&[1], LBA_ROOT_DIR, ISO_SECTOR as u32, true),
        dir_record(CNF_NAME, LBA_SYSTEM_CNF, CNF.len() as u32, false),
        dir_record(ISO_EXE_NAME, LBA_EXE, exe.len() as u32, false),
    ] {
        root_dir.extend_from_slice(&rec);
    }

    // The path table, listing the one directory there is: a one-byte name (the
    // root's, which is the single zero byte), parented to itself.
    let mut path_l = vec![1u8, 0];
    path_l.extend_from_slice(&LBA_ROOT_DIR.to_le_bytes());
    path_l.extend_from_slice(&1u16.to_le_bytes());
    path_l.extend_from_slice(&[0, 0]); // name, then padding to an even length
    let mut path_m = vec![1u8, 0];
    path_m.extend_from_slice(&LBA_ROOT_DIR.to_be_bytes());
    path_m.extend_from_slice(&1u16.to_be_bytes());
    path_m.extend_from_slice(&[0, 0]);

    let root_record = dir_record(&[0], LBA_ROOT_DIR, ISO_SECTOR as u32, true);
    let pvd = primary_volume_descriptor(total_sectors, path_l.len() as u32, &root_record);

    let mut terminator = vec![0u8; ISO_SECTOR];
    terminator[0] = 0xff; // volume descriptor set terminator
    terminator[1..6].copy_from_slice(b"CD001");
    terminator[6] = 1;

    let mut image = vec![0u8; total_sectors as usize * ISO_SECTOR];
    let mut put = |lba: u32, bytes: &[u8]| {
        let at = lba as usize * ISO_SECTOR;
        image[at..at + bytes.len()].copy_from_slice(bytes);
    };
    put(LBA_PVD, &pvd);
    put(LBA_TERMINATOR, &terminator);
    put(LBA_PATH_TABLE_L, &path_l);
    put(LBA_PATH_TABLE_M, &path_m);
    put(LBA_ROOT_DIR, &root_dir);
    put(LBA_SYSTEM_CNF, CNF.as_bytes());
    put(LBA_EXE, exe);
    image
}

/// Wrap the PlayStation executable at `exe_path` in a bootable disc image, so it
/// can be handed to pcsx_rearmed, which loads discs but refuses a raw PS-X EXE.
/// Beetle takes the executable directly but needs a real BIOS, which is the
/// thing this avoids having to ask for.
///
/// The image is cached under its contents, since the file it came from is often
/// unpacked to a fresh temp directory on every launch — see [`prepare_psx_disc`],
/// which keys its cache the same way and for the same reason.
///
/// Returns `None` when `exe_path` isn't a PlayStation executable at all.
pub fn create_psx_iso(exe_path: &Path) -> Result<Option<PathBuf>> {
    use std::hash::{Hash, Hasher};

    let mut exe = fs::read(exe_path)?;
    if exe.len() <= PSX_HEADER_LEN as usize || exe[0..8] != *b"PS-X EXE" {
        return Ok(None);
    }
    // The header's text size has to describe what follows it exactly, and the
    // disc is read straight through from the executable's first sector, so a
    // size that overruns the file would pull in whatever sectors come after it.
    // Same repair the raw-executable path makes, applied to our copy — the file
    // we were handed isn't ours to write to.
    if let Some(size) = psx_text_size_fix(exe_path) {
        debug!("Recording a {size:#x} byte text section for {exe_path:?}");
        exe[PSX_TEXT_SIZE_OFFSET..PSX_TEXT_SIZE_OFFSET + 4].copy_from_slice(&size.to_le_bytes());
        exe.truncate(PSX_HEADER_LEN as usize + size as usize);
    }

    let mut key = std::collections::hash_map::DefaultHasher::new();
    exe.hash(&mut key);
    let stem = exe_path.file_stem().unwrap_or_default().to_string_lossy();
    let out_dir = dirs::cache_dir()
        .unwrap_or_default()
        .join("demarc")
        .join("psxexe");
    let out = out_dir.join(format!("{stem}-{:016x}.iso", key.finish()));
    if out.is_file() {
        debug!("Using cached disc image {out:?}");
        return Ok(Some(out));
    }

    info!("Building bootable disc image for {exe_path:?}");
    fs::create_dir_all(&out_dir)?;
    // Write beside the target and rename, so a second demarc looking at the
    // cache never finds a half-written image under a name that says it is done.
    let partial = out.with_extension("iso.part");
    fs::write(&partial, build_psx_iso(&exe))?;
    fs::rename(&partial, &out)?;
    Ok(Some(out))
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

/// True if `path` is a raw PlayStation executable rather than a disc image. No
/// core loads one the way it stands: [`create_psx_iso`] wraps it in a disc for
/// the default core, and Beetle takes it directly — see `get_core`.
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

/// The header a ROM copier — a Super Wild Card and its clones — writes in front
/// of the dump it made. Emulators skip it, but it is worth recognising: a scene
/// release that has had its cartridge header blanked may have nothing else left
/// to identify it by.
const COPIER_HEADER_LEN: u64 = 0x200;

/// The copier header's signature at offset 8, with the machine it dumped in the
/// third byte — `0x04` is Super Nintendo, `0x06` the Megadrive.
const COPIER_MAGIC_SNES: [u8; 3] = [0xaa, 0xbb, 0x04];

/// Where a Super Nintendo cartridge keeps its 64-byte internal header, measured
/// from the start of the ROM data: the last page of the first bank on a LoROM,
/// of the second bank on a HiROM, and 4MB in on the rare ExHiROM.
const SNES_HEADER_OFFSETS: [u64; 3] = [0x7fc0, 0xffc0, 0x40_ffc0];

/// The unit a Super Nintendo ROM is always a whole number of: one bank.
const SNES_BANK_SIZE: u64 = 0x8000;

/// True if `header` — 64 bytes read from one of [`SNES_HEADER_OFFSETS`] — is a
/// Super Nintendo cartridge header.
///
/// Everything else in it is advisory: scene releases routinely leave the title
/// blank, the map mode zero and the ROM size field describing some other cart.
/// The two fields that still have to hold are the checksum at 0x1e and its
/// complement at 0x1c, which add up to 0xffff, and the emulation-mode reset
/// vector at 0x3c, which has to point at the ROM half of a bank. A ROM with a
/// zeroed header fails this and is caught by the copier header instead.
fn is_snes_header(header: &[u8]) -> bool {
    if header.len() < 0x40 {
        return false;
    }
    let word = |o: usize| u16::from_le_bytes([header[o], header[o + 1]]);
    // A pair adding up to 0xffff is exactly a pair that is each other's
    // complement, and xor says so without worrying about the carry. A checksum
    // of zero passes that test against 0xffff but describes an empty ROM, so
    // it is the one value ruled out.
    word(0x1c) ^ word(0x1e) == 0xffff && word(0x1e) != 0 && word(0x3c) >= 0x8000
}

/// True if `path` is a Super Nintendo ROM image.
///
/// A ROM is a whole number of 32K banks, optionally behind a copier header, and
/// is recognised either by that header's signature or by a cartridge header at
/// one of the three places the machine looks for one. Both paths are needed:
/// the copier header is the only thing left in a dump whose cartridge header
/// was blanked, and plenty of ROMs ship without a copier header at all.
pub fn is_snes_rom(path: &Path) -> bool {
    /// Past this a file is some other kind of image: no cartridge ever shipped
    /// with more than 8MB in it, ExHiROM ones included.
    const MAX_ROM_SIZE: u64 = 16 * 1024 * 1024;

    let Ok(len) = fs::metadata(path).map(|m| m.len()) else {
        return false;
    };
    let copier = match len % SNES_BANK_SIZE {
        0 => 0,
        COPIER_HEADER_LEN => COPIER_HEADER_LEN,
        _ => return false,
    };
    let rom_size = len - copier;
    if rom_size == 0 || len > MAX_ROM_SIZE {
        return false;
    }
    if copier != 0
        && read_at(path, 8, COPIER_MAGIC_SNES.len()).is_ok_and(|m| m == COPIER_MAGIC_SNES)
    {
        return true;
    }
    SNES_HEADER_OFFSETS
        .iter()
        .filter(|&&offset| offset + 0x40 <= rom_size)
        .any(|&offset| read_at(path, copier + offset, 0x40).is_ok_and(|h| is_snes_header(&h)))
}

/// Read up to `len` bytes from the start of `path`. Returns fewer bytes if the
/// file is shorter.
pub fn read_header(path: &Path, len: usize) -> std::io::Result<Vec<u8>> {
    read_at(path, 0, len)
}

/// Read up to `len` bytes of `path` starting at `offset`. Returns fewer bytes
/// if the file ends first.
fn read_at(path: &Path, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut buf = vec![0u8; len];
    let mut file = fs::File::open(path)?;
    if offset != 0 {
        file.seek(SeekFrom::Start(offset))?;
    }
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

/// The entry of `dir` named `name`, whatever its case — Atari and Amiga file
/// systems are case insensitive, so a release's `AUTO` folder may just as well
/// be spelled `auto` on disk.
pub fn find_child(dir: &Path, name: &str) -> Option<PathBuf> {
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

        if path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .starts_with(".")
        {
            continue;
        }
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

    /// A ROM of `banks` 32K banks, with a cartridge header written at `offset`
    /// unless that is `None`, optionally behind a copier header.
    fn snes_rom(
        dir: &Path,
        name: &str,
        banks: usize,
        copier: bool,
        offset: Option<usize>,
    ) -> PathBuf {
        let mut rom = vec![0u8; banks * SNES_BANK_SIZE as usize];
        if let Some(offset) = offset {
            rom[offset..offset + 21].copy_from_slice(b"DEMO                 ");
            // Checksum 0x1234 with its complement, then a reset vector.
            rom[offset + 0x1c..offset + 0x20].copy_from_slice(&[0xcb, 0xed, 0x34, 0x12]);
            rom[offset + 0x3c..offset + 0x3e].copy_from_slice(&[0x00, 0x80]);
        }
        if copier {
            let mut header = vec![0u8; COPIER_HEADER_LEN as usize];
            header[8..11].copy_from_slice(&COPIER_MAGIC_SNES);
            header.extend_from_slice(&rom);
            rom = header;
        }
        let path = dir.join(name);
        fs::write(&path, &rom).unwrap();
        path
    }

    /// The cartridge header sits in a different bank on each mapping, and a
    /// broken checksum pair is not a ROM.
    #[test]
    fn snes_rom_detected_by_cartridge_header() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        // LoROM, HiROM, and the same two behind a copier header.
        assert!(is_snes_rom(&snes_rom(
            dir.path(),
            "lo",
            2,
            false,
            Some(0x7fc0)
        )));
        assert!(is_snes_rom(&snes_rom(
            dir.path(),
            "hi",
            4,
            false,
            Some(0xffc0)
        )));
        assert!(is_snes_rom(&snes_rom(
            dir.path(),
            "lo.hdr",
            2,
            true,
            Some(0x7fc0)
        )));

        // Nothing at either place, and no copier header to fall back on.
        assert!(!is_snes_rom(&snes_rom(dir.path(), "empty", 2, false, None)));

        // A header whose checksum and complement don't agree.
        let path = snes_rom(dir.path(), "bad.sum", 2, false, Some(0x7fc0));
        let mut rom = fs::read(&path).unwrap();
        rom[0x7fc0 + 0x1e] = 0x35;
        fs::write(&path, &rom).unwrap();
        assert!(!is_snes_rom(&path));

        // A header pointing its reset vector at RAM rather than ROM.
        let path = snes_rom(dir.path(), "bad.vector", 2, false, Some(0x7fc0));
        let mut rom = fs::read(&path).unwrap();
        rom[0x7fc0 + 0x3d] = 0x1f;
        fs::write(&path, &rom).unwrap();
        assert!(!is_snes_rom(&path));
    }

    /// Cracked releases hand out ROMs with the cartridge header wiped, so the
    /// copier header in front of them is all that is left to go on.
    #[test]
    fn snes_rom_detected_by_copier_header() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let path = snes_rom(dir.path(), "blanked", 1, true, None);
        assert!(is_snes_rom(&path));

        // The same header, from a Megadrive copier.
        let mut rom = fs::read(&path).unwrap();
        rom[10] = 0x06;
        fs::write(&path, &rom).unwrap();
        assert!(!is_snes_rom(&path));

        // Copier header, but the rest is not a whole number of banks.
        rom.truncate(rom.len() - 1);
        let path = dir.path().join("short");
        fs::write(&path, &rom).unwrap();
        assert!(!is_snes_rom(&path));
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
        // Both halves matter: the type picks the system, `is_psx_exe` decides
        // the executable has to be wrapped in a disc before a core sees it.
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

    /// Walk an image the way the console's boot code does: the volume descriptor
    /// at sector 16, the root directory record inside it at offset 156, then the
    /// records in the directory that points to. Returns each file — directories
    /// are the `.`/`..` entries and nothing else here — with its contents, taken
    /// at the length the record claims.
    fn read_iso_files(image: &[u8]) -> Vec<(String, Vec<u8>)> {
        let sector = |lba: u32| &image[lba as usize * ISO_SECTOR..];
        let le32 = |b: &[u8]| u32::from_le_bytes(b[0..4].try_into().unwrap());

        let pvd = sector(LBA_PVD);
        assert_eq!(pvd[0], 1, "volume descriptor type");
        assert_eq!(&pvd[1..6], b"CD001", "standard identifier");
        // Every field is stored twice; a reader taking the big-endian half has
        // to see the same numbers as one taking the little-endian half.
        assert_eq!(
            le32(&pvd[80..]),
            u32::from_be_bytes(pvd[84..88].try_into().unwrap())
        );

        let root = &pvd[156..];
        let mut dir = sector(le32(&root[2..]));
        let mut end = le32(&root[10..]) as usize;

        let mut files = vec![];
        while end > 0 && dir[0] != 0 {
            let rec = &dir[..dir[0] as usize];
            let name_len = rec[32] as usize;
            let name = String::from_utf8_lossy(&rec[33..33 + name_len]).to_string();
            if rec[25] & 0x02 == 0 {
                let (lba, len) = (le32(&rec[2..]), le32(&rec[10..]) as usize);
                files.push((name, sector(lba)[..len].to_vec()));
            }
            end -= rec.len();
            dir = &dir[rec.len()..];
        }
        files
    }

    /// The disc has to name the executable in `SYSTEM.CNF` and carry it byte for
    /// byte, since the core reads it straight off the disc as a PS-X EXE — the
    /// header sector included.
    #[test]
    fn psx_iso_boots_the_executable() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        // The cached image is named after the executable, so each test that
        // builds one uses a stem of its own and can clean up after itself.
        let exe = dir.path().join("bootable.psx");
        write_psx_exe(&exe, 0x8001_0000, 0x4000, 0x4000);

        let iso = create_psx_iso(&exe).unwrap().expect("should be wrapped");
        let image = fs::read(&iso).unwrap();
        let _ = fs::remove_file(&iso);

        assert_eq!(image.len() % ISO_SECTOR, 0, "whole sectors");
        assert_eq!(
            &image[LBA_PVD as usize * ISO_SECTOR + 8..][..11],
            b"PLAYSTATION"
        );

        let files = read_iso_files(&image);
        let names: Vec<_> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["SYSTEM.CNF;1", "PSX.EXE;1"]);

        let cnf = String::from_utf8_lossy(&files[0].1).to_string();
        assert!(
            cnf.contains("BOOT = cdrom:\\PSX.EXE;1"),
            "boot line missing from: {cnf}"
        );
        assert_eq!(files[1].1, fs::read(&exe).unwrap(), "executable on disc");
    }

    /// An executable whose header undercounts its text section is unloadable
    /// from a disc too — the core reads the recorded size and stops there — so
    /// the copy that goes on the disc records what is really behind it.
    #[test]
    fn psx_iso_records_the_real_text_size() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let exe = dir.path().join("short.psx");
        write_psx_exe(&exe, 0x8001_0000, 0x800, 0x4000);

        let iso = create_psx_iso(&exe).unwrap().unwrap();
        let image = fs::read(&iso).unwrap();
        let _ = fs::remove_file(&iso);

        let on_disc = read_iso_files(&image).pop().unwrap().1;
        assert_eq!(
            u32::from_le_bytes(on_disc[PSX_TEXT_SIZE_OFFSET..][..4].try_into().unwrap()),
            0x4000
        );
        assert_eq!(on_disc.len(), PSX_HEADER_LEN as usize + 0x4000);
        // The original is left exactly as it was; only the copy is repaired.
        assert!(psx_needs_text_fix(&exe));
    }

    /// pcsx_rearmed decides whether an image has 2048- or 2352-byte sectors from
    /// its length, so a length divisible by both would be read as the wrong kind
    /// of disc. That is every 147th sector, and the image has to avoid landing
    /// there whatever size the executable is.
    #[test]
    fn psx_iso_length_cannot_be_mistaken_for_raw_sectors() {
        // 125 sectors of executable puts the image at exactly 147 without the
        // padding sector; the neighbours on either side are the control.
        for sectors in 124..=126 {
            let mut exe = b"PS-X EXE".to_vec();
            exe.resize(sectors * ISO_SECTOR, 0);
            let image = build_psx_iso(&exe);
            assert_eq!(image.len() % ISO_SECTOR, 0);
            assert_ne!(
                image.len() % 2352,
                0,
                "{sectors} sector executable makes an ambiguous image"
            );
        }
    }

    /// Discs get unpacked to a fresh temp directory on every launch, so the
    /// cache has to be keyed on the executable's contents rather than where it
    /// happened to be — the same reason [`prepare_psx_disc`] does.
    #[test]
    fn psx_iso_cache_key_ignores_path() {
        let mut seen = vec![];
        for _ in 0..2 {
            let dir = tempfile::Builder::new()
                .prefix("demarc-")
                .tempdir()
                .unwrap();
            let exe = dir.path().join("same.psx");
            write_psx_exe(&exe, 0x8001_0000, 0x1000, 0x1000);
            seen.push(create_psx_iso(&exe).unwrap().unwrap());
        }
        assert_eq!(
            seen[0], seen[1],
            "identical executables must share one cached image"
        );
        for iso in seen {
            let _ = fs::remove_file(iso);
        }
    }

    /// Anything that isn't a PlayStation executable is left for the caller to
    /// deal with, rather than wrapped in a disc that can't boot.
    #[test]
    fn non_executables_are_not_wrapped() {
        let dir = tempfile::Builder::new()
            .prefix("demarc-")
            .tempdir()
            .unwrap();
        let plain = dir.path().join("data.bin");
        fs::write(&plain, [0u8; 0x2000]).unwrap();
        assert!(create_psx_iso(&plain).unwrap().is_none());

        // A header with nothing behind it is a PS-X EXE with no program in it.
        let empty = dir.path().join("empty.psx");
        write_psx_exe(&empty, 0x8001_0000, 0, 0);
        assert!(create_psx_iso(&empty).unwrap().is_none());
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
