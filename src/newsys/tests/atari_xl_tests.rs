use super::*;

/// The two real headers this was written against, one 128- and one
/// 256-byte-sector image, against the Spectrum attribute dump that shares
/// the extension — 768 bytes of one repeated colour, with no header at all.
#[test]
fn tells_a_disk_from_a_spectrum_attribute_dump() {
    assert!(is_atari_disk(&[0x96, 0x02, 0x80, 0x20, 0x80, 0x00]));
    assert!(is_atari_disk(&[0x96, 0x02, 0xe8, 0x2c, 0x00, 0x01]));
    assert!(!is_atari_disk(&[0x09; HEADER_LEN]));
    // Right magic, but no sector size the format defines.
    assert!(!is_atari_disk(&[0x96, 0x02, 0x80, 0x20, 0x40, 0x00]));
}
