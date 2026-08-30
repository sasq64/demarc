use std::path::Path;

use crate::newsys::get_ext;

use super::System;
use super::utils::read_at;

const CORE_NAME_FAKE08: &str = "fake08";

/// A PICO-8 label cart is a PNG of one fixed size — the 128x128 screen inside
/// its border — carrying the cart itself in the low bits of the pixels. fake-08
/// rejects a PNG of any other size outright, so testing the size here is the
/// same test the core goes on to apply.
const CART_PNG_SIZE: (u32, u32) = (160, 205);

/// PNG signature (8 bytes) + the IHDR length and type (8) + width and height (8).
const PNG_HEADER_LEN: usize = 24;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub struct Pico8System {}

/// Whether `header` starts a PNG the size of a PICO-8 cart.
///
/// The extension can't decide this on its own: carts are distributed as
/// `.p8.png`, but every screenshot beside them in a release directory is a
/// `.png` too, and those belong to [`ImageSystem`](super::images::ImageSystem).
/// The dimensions in the IHDR are what tell the two apart.
fn is_cart_png(header: &[u8]) -> bool {
    if header.len() < PNG_HEADER_LEN || header[0..8] != PNG_SIGNATURE || &header[12..16] != b"IHDR"
    {
        return false;
    }
    let width = u32::from_be_bytes([header[16], header[17], header[18], header[19]]);
    let height = u32::from_be_bytes([header[20], header[21], header[22], header[23]]);
    (width, height) == CART_PNG_SIZE
}

impl System for Pico8System {
    fn extensions(&self) -> &'static [&'static str] {
        &["p8"]
    }

    fn is_console(&self) -> bool {
        true
    }

    fn can_load(&self, path: &Path) -> bool {
        // `.p8` is PICO-8's own extension and nothing else here wants it, so it
        // stands on its own; a `.png` has to prove it is a cart.
        self.handles_ext(path)
            || (get_ext(path) == "png"
                && read_at(path, 0, PNG_HEADER_LEN).is_ok_and(|h| is_cart_png(&h)))
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_FAKE08
    }

    fn name(&self) -> &'static str {
        "Pico-8"
    }
}

#[cfg(test)]
mod tests {
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
}
