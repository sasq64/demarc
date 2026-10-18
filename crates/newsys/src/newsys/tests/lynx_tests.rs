use super::*;

#[test]
fn tells_a_lynx_cart_from_a_c64_archive() {
    assert!(is_lynx_file("lnx", b"LYNX\0\x02"));
    // C64 Lynx archive: BASIC stub loading at $0801.
    assert!(!is_lynx_file("lnx", &[0x01, 0x08, 0x5b, 0x08]));
}

#[test]
fn needs_the_bll_magic_on_an_o_file() {
    assert!(is_lynx_file("o", &[0x80, 0x08, 0x02, 0x00]));
    assert!(!is_lynx_file("o", b"\x7fELF"));
    assert!(!is_lynx_file("bin", b"LYNX"));
}
