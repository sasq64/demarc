use anyhow::{Result, bail};

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use unarc_rs::unified::ArchiveFormat;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::systems::{SystemType, get_system_type};

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

}
