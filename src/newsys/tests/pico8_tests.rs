use super::*;

/// Build the first 24 bytes of a PNG declaring `width` x `height`.
fn png_header(width: u32, height: u32) -> Vec<u8> {
    let mut header = PNG_SIGNATURE.to_vec();
    header.extend_from_slice(&13u32.to_be_bytes());
    header.extend_from_slice(b"IHDR");
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header
}

#[test]
fn tells_a_cart_from_a_screenshot() {
    assert!(is_cart_png(&png_header(160, 205)));
    // The screenshots that sit beside a cart: PICO-8's own screen size, and
    // a scaled-up grab of it.
    assert!(!is_cart_png(&png_header(128, 128)));
    assert!(!is_cart_png(&png_header(640, 820)));
    // Right size, but not a PNG at all.
    assert!(!is_cart_png(&[0u8; PNG_HEADER_LEN]));
    assert!(!is_cart_png(&png_header(160, 205)[..PNG_HEADER_LEN - 1]));
}
