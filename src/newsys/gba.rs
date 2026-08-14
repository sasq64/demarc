use std::path::Path;

use crate::{Args, newsys::utils::read_header};

use super::System;

const CORE_NAME_GBA: &str = "mgba";

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

pub struct GBASystem {}

impl GBASystem {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for GBASystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["gba", "agb"]
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path)
            || read_header(path, GBA_HEADER_LEN)
                .map(|d| is_gba_rom(&d))
                .unwrap_or(false)
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_GBA
    }
    fn name(&self) -> &'static str {
        "GBA"
    }
}
