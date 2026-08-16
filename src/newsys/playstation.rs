use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};
use tracing::{debug, info, warn};

use super::utils::read_header;

use crate::{newsys::walk_dir, workfile::WorkFile};

use super::System;

const CORE_NAME_PSX: &str = "pcsx_rearmed";
pub struct PSXSystem {}

impl PSXSystem {}

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

pub fn is_psx_exe(path: &Path) -> bool {
    read_header(path, 8).is_ok_and(|h| h == b"PS-X EXE")
}

/// The sector layouts a disc image turns up in, as the bytes one sector takes
/// in the file and where its 2048 bytes of user data start inside that.
///
/// A `.iso` holds the user data and nothing else. The `.bin` a cue names holds
/// whole CD sectors: a 12-byte sync pattern and a 4-byte address in front, then
/// — for Mode 2, which is what PlayStation discs are pressed as — an 8-byte XA
/// subheader, with error correction after the data. A CloneCD `.img` adds 96
/// bytes of subchannel per sector on top of that.
///
/// Order matters: the first layout whose sector 16 looks like a volume
/// descriptor is the one taken, and Mode 2 is the common case here.
const SECTOR_LAYOUTS: &[(u64, usize)] = &[
    (2352, 24), // Mode 2 Form 1
    (2352, 16), // Mode 1
    (2048, 0),  // user data only
    (2336, 8),  // Mode 2 with the sync and address stripped
    (2448, 24), // Mode 2 Form 1 plus subchannel
    (2448, 16), // Mode 1 plus subchannel
];

/// Sectors of the root directory [`DiscImage::root_names`] will read before
/// giving up. Only enough to see the boot files matters, and a corrupt length
/// field shouldn't turn a sniff into a long read.
const MAX_ROOT_SECTORS: usize = 16;

/// A pressed PlayStation disc keeps Sony's licence text in the system area, the
/// 16 sectors ahead of the filesystem. It is the one marker that needs no
/// filesystem at all — but scene images often have it stripped, since a rip of
/// just the data track tends to zero it, so its absence proves nothing.
const PSX_LICENCE: &[u8] = b"Sony Computer Entertainment";

/// A disc image opened in whatever sector layout it turned out to be stored in.
/// One only exists for a file that really holds an ISO9660 filesystem, since
/// finding the volume descriptor is what identifies the layout in the first
/// place.
struct DiscImage {
    file: fs::File,
    sector_size: u64,
    data_offset: usize,
}

impl DiscImage {
    /// Open `path` as a disc image, or `None` if no [`SECTOR_LAYOUTS`] entry
    /// puts a primary volume descriptor at sector 16 — which is to say, if it
    /// isn't a data disc at all.
    fn open(path: &Path) -> Option<Self> {
        let mut disc = DiscImage {
            file: fs::File::open(path).ok()?,
            sector_size: 0,
            data_offset: 0,
        };
        for &(sector_size, data_offset) in SECTOR_LAYOUTS {
            disc.sector_size = sector_size;
            disc.data_offset = data_offset;
            if disc
                .read_sector(LBA_PVD)
                .is_some_and(|pvd| pvd[0] == 1 && pvd[1..6] == *b"CD001")
            {
                return Some(disc);
            }
        }
        None
    }

    /// The user data of sector `lba`, or `None` past the end of the image.
    fn read_sector(&mut self, lba: u32) -> Option<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let at = lba as u64 * self.sector_size + self.data_offset as u64;
        let mut buf = vec![0u8; ISO_SECTOR];
        self.file.seek(SeekFrom::Start(at)).ok()?;
        self.file.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    /// Whether the system area carries [`PSX_LICENCE`].
    fn has_licence(&mut self) -> bool {
        (0..LBA_PVD).any(|lba| {
            self.read_sector(lba)
                .is_some_and(|sector| sector.windows(PSX_LICENCE.len()).any(|w| w == PSX_LICENCE))
        })
    }

    /// The names in the root directory, upper cased and with the `;1` version
    /// suffix dropped.
    ///
    /// This walks the directory itself rather than going through a filesystem
    /// crate: every crate on offer reads a plain 2048-byte-sector image, so a
    /// raw MODE2/2352 track — which is what a PlayStation disc actually is —
    /// would need the sector translation above wrapped around it anyway, and
    /// the question here is only which names the root holds.
    fn root_names(&mut self) -> Vec<String> {
        let Some(pvd) = self.read_sector(LBA_PVD) else {
            return vec![];
        };
        // The root's own directory record sits inside the volume descriptor, at
        // the fixed offset every reader picks it up from. See [`dir_record`] for
        // the layout of the fields read here.
        let root = &pvd[156..190];
        let field = |at: usize| u32::from_le_bytes(root[at..at + 4].try_into().unwrap());
        let lba = field(2);
        let sectors = (field(10) as usize)
            .div_ceil(ISO_SECTOR)
            .min(MAX_ROOT_SECTORS);

        let mut names = vec![];
        for i in 0..sectors as u32 {
            let Some(sector) = self.read_sector(lba + i) else {
                break;
            };
            let mut at = 0;
            // Records never straddle a sector; the leftover is zeroed, and a
            // zero length is what marks the end of the ones in this sector.
            while at + 33 < ISO_SECTOR {
                let rec_len = sector[at] as usize;
                let name_len = sector[at + 32] as usize;
                if rec_len < 34 || at + rec_len > ISO_SECTOR || 33 + name_len > rec_len {
                    break;
                }
                // The one-byte names 0 and 1 are `.` and `..`, which every
                // directory has and no file is called.
                if name_len > 1 {
                    let name = String::from_utf8_lossy(&sector[at + 33..at + 33 + name_len]);
                    names.push(name.split(';').next().unwrap_or_default().to_uppercase());
                }
                at += rec_len;
            }
        }
        names
    }
}

/// Whether `path` is a PlayStation disc image — the data track of a cue/bin, an
/// `.iso`, or the same thing under any other name.
///
/// Either of two markers is enough. Sony's licence text in the system area
/// needs no filesystem, but only survives on a full raw dump. What every disc
/// the console boots has is `SYSTEM.CNF` in the root — or, when the boot file is
/// left at its default name, `PSX.EXE` — and that is also what separates a
/// PlayStation disc from a Saturn or PC Engine one arriving in the same
/// MODE2/2352 wrapper.
pub fn is_psx_disc(path: &Path) -> bool {
    let Some(mut disc) = DiscImage::open(path) else {
        return false;
    };
    if disc.has_licence() {
        return true;
    }
    disc.root_names()
        .iter()
        .any(|name| name == "SYSTEM.CNF" || name == "PSX.EXE")
}

/// Whether the cue sheet at `path` describes a PlayStation disc, judged by the
/// data track it names. A sheet holding nothing but audio tracks is a CD, and
/// one whose data track belongs to another console isn't ours either.
pub fn is_psx_cue(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    parse_cue_files(&text)
        .iter()
        .filter(|f| {
            ["BINARY", "MOTOROLA"]
                .iter()
                .any(|k| f.kind.eq_ignore_ascii_case(k))
        })
        .filter_map(|f| resolve_ignoring_case(dir, &f.name))
        .any(|track| is_psx_disc(&track))
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

fn handle_exe(work_file: &Path) -> Result<PathBuf> {
    // Only Beetle loads a raw PS-X EXE, and only with a real BIOS. Wrap
    // the executable in a bootable disc image instead and the default
    // core takes it, HLE BIOS and all. The image lives in the cache and
    // is reused between runs, so it deliberately isn't the `temp_dir`.

    debug!("Building iso");
    let disc = create_psx_iso(work_file).unwrap_or_else(|err| {
        warn!("Could not build a disc image for {work_file:?}: {err}");
        None
    });
    if let Some(disc) = disc {
        debug!("FMT: PSX executable wrapped in disc image {disc:?}");
        return Ok(disc);
    } else {
        //work_file.meta.insert("psx_core".into(), "beetle".into());
        // A scene exe whose header undercounts its text section doesn't
        // load at all, so patch the header before the core sees it. The
        // file we were handed isn't ours to write to unless it already
        // came out of a temp directory, so anything else is copied first.
        // if psx_needs_text_fix(&work_file) {
        //     if temp_dir.is_none() {
        //         // TODO: temp file leek? Better to wire this in convert_dir()
        //         let dir = tempfile::Builder::new().prefix("demarc-").tempdir()?;
        //         let copy = dir.path().join(path.file_name().unwrap());
        //         fs::copy(&path, &copy)?;
        //         path = copy;
        //         temp_dir = Some(dir);
        //     }
        //     debug!("FMT: patching short PSX text section in {path:?}");
        //     fix_psx_text_size(&path)?;
        //}
    }
    bail!("Could not load EXE");
}
impl System for PSXSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_PSX
    }

    fn name(&self) -> &'static str {
        "PSX"
    }

    fn is_console(&self) -> bool {
        true
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("pcsx_rearmed_bios", "HLE"),
            ("pcsx_rearmed_region", "PAL"),
            ("beetle_psx_region", "pal"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut cue = None;
        let mut disc = None;
        let mut exe = None;

        walk_dir(&file.path.clone(), 4, |path, ext, _header| {
            if is_psx_exe(path) {
                if exe.is_none() {
                    // A broken executable shouldn't take the whole release down
                    // with it; there may still be a disc image next to it.
                    match handle_exe(path) {
                        Ok(built) => exe = Some(built),
                        Err(err) => warn!("Could not use PSX executable {path:?}: {err}"),
                    }
                }
            } else if ext == "cue" {
                if cue.is_none() && is_psx_cue(path) {
                    cue = Some(path.to_owned());
                }
            } else if disc.is_none() && is_psx_disc(path) {
                disc = Some(path.to_owned());
            }
            Ok(())
        })?;

        // A cue describes the whole disc — the data track plus any CD audio — so
        // it wins over the track it names. The wrapped executable comes last: a
        // release shipping both has the disc image as the real thing.
        let found = match cue {
            Some(cue) => Some(prepare_psx_disc(&cue)?.unwrap_or(cue)),
            None => disc.or(exe),
        };

        if let Some(found) = found {
            debug!("FMT: PSX disc {found:?}");
            file.path = found;
            return Ok(true);
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn testdata() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("psx")
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A data disc with nothing on it the PlayStation would boot — what another
    /// console's disc looks like from the outside, down to the MODE2/2352
    /// wrapper when `raw` is set.
    fn other_disc(dir: &Path, name: &str, raw: bool) -> PathBuf {
        let mut root_dir = Vec::new();
        for rec in [
            dir_record(&[0], LBA_ROOT_DIR, ISO_SECTOR as u32, true),
            dir_record(&[1], LBA_ROOT_DIR, ISO_SECTOR as u32, true),
            dir_record(b"DATA.BIN;1", LBA_ROOT_DIR + 1, ISO_SECTOR as u32, false),
        ] {
            root_dir.extend_from_slice(&rec);
        }
        let total = LBA_ROOT_DIR + 2;
        let root = dir_record(&[0], LBA_ROOT_DIR, ISO_SECTOR as u32, true);
        let pvd = primary_volume_descriptor(total, 10, &root);

        let mut image = vec![0u8; total as usize * ISO_SECTOR];
        image[LBA_PVD as usize * ISO_SECTOR..][..ISO_SECTOR].copy_from_slice(&pvd);
        image[LBA_ROOT_DIR as usize * ISO_SECTOR..][..root_dir.len()].copy_from_slice(&root_dir);

        if raw {
            // Wrap each sector the way a cue's bin does: sync pattern, address,
            // XA subheader, user data, then room for the error correction.
            let mut sectors = vec![0u8; total as usize * 2352];
            for lba in 0..total as usize {
                let at = lba * 2352;
                sectors[at] = 0;
                sectors[at + 1..at + 12].fill(0xff);
                sectors[at + 15] = 2;
                sectors[at + 24..at + 24 + ISO_SECTOR]
                    .copy_from_slice(&image[lba * ISO_SECTOR..][..ISO_SECTOR]);
            }
            image = sectors;
        }

        let path = dir.join(name);
        fs::write(&path, image).unwrap();
        path
    }

    /// The two markers, one each: a raw dump that still carries Sony's licence
    /// text, and a data track whose system area was stripped, leaving only the
    /// boot file in the root to go on.
    #[test]
    fn detects_psx_discs() {
        assert!(is_psx_disc(
            &testdata().join("thisispsx/thisispsx_rc1c_iso.bin")
        ));
        assert!(is_psx_disc(&testdata().join("monophobia/mono_t1.bin")));
        assert!(is_psx_cue(&testdata().join("monophobia/mono.cue")));
    }

    /// A wrapped executable has to come back out as a disc the same detection
    /// accepts, or the release would only load the once.
    #[test]
    fn wrapped_executable_is_a_psx_disc() {
        let iso = create_psx_iso(&testdata().join("paradox/pdx-051.psx"))
            .unwrap()
            .expect("paradox release is a PS-X EXE");
        assert!(is_psx_disc(&iso));
    }

    /// Everything that isn't one: another console's disc in either wrapper, a
    /// cue naming it, an audio-only cue, and a file that is no disc at all.
    #[test]
    fn rejects_other_discs() {
        let dir = temp_dir("psx_detect_test");
        let iso = other_disc(&dir, "other.iso", false);
        let bin = other_disc(&dir, "other.bin", true);
        // Both are readable data discs — rejected on what's on them, not
        // because the layout sniff gave up on them.
        assert!(DiscImage::open(&iso).is_some());
        assert!(DiscImage::open(&bin).is_some());
        assert!(!is_psx_disc(&iso));
        assert!(!is_psx_disc(&bin));

        let cue = dir.join("other.cue");
        fs::write(&cue, "FILE \"other.bin\" BINARY\n  TRACK 01 MODE2/2352\n").unwrap();
        assert!(!is_psx_cue(&cue));

        fs::write(dir.join("track.wav"), b"not really a wav").unwrap();
        let audio = dir.join("audio.cue");
        fs::write(&audio, "FILE \"track.wav\" WAVE\n  TRACK 01 AUDIO\n").unwrap();
        assert!(!is_psx_cue(&audio));

        assert!(!is_psx_disc(&testdata().join("paradox/pdx-051.JPG")));
    }
}
