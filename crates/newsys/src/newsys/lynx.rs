use std::path::Path;

use crate::{newsys::get_ext, utils::read_header};

use super::System;

const CORE_NAME_LYNX: &str = "holani";

/// `.lnx` is shared with C64 Lynx archives, which open with a BASIC stub
/// instead of the `LYNX` magic; `.o` is a BLL homebrew binary.
fn is_lynx_file(ext: &str, header: &[u8]) -> bool {
    match ext {
        "lnx" => header.starts_with(b"LYNX"),
        "o" => header.starts_with(&[0x80, 0x08]),
        _ => false,
    }
}

pub struct LynxSystem {}

impl System for LynxSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["lnx", "o"]
    }
    fn is_console(&self) -> bool {
        true
    }

    fn can_load(&self, path: &Path) -> bool {
        read_header(path, 4)
            .map(|h| is_lynx_file(&get_ext(path), &h))
            .unwrap_or(false)
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_LYNX
    }
    fn name(&self) -> &'static str {
        "Lynx"
    }
}

#[cfg(test)]
#[path = "tests/lynx_tests.rs"]
mod tests;
