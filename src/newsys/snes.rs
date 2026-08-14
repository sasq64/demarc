use std::{fs, path::Path};

use crate::Args;

use super::System;
use super::utils::read_at;

const CORE_NAME_SNES: &str = "bsnes";

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

pub struct SNESSystem {}

impl SNESSystem {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for SNESSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["smc", "sfc", "swc", "fig"]
    }
    fn is_console(&self) -> bool {
        true
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path) || is_snes_rom(path)
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_SNES
    }
    fn name(&self) -> &'static str {
        "SNES"
    }
}
