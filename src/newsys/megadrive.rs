use std::path::Path;

use crate::{Args, newsys::utils::read_header};

use super::System;

const CORE_NAME_MD: &str = "picodrive";

pub struct MegadriveSystem {}

impl MegadriveSystem {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for MegadriveSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["32x", "gen", "smd"]
    }
    fn is_console(&self) -> bool {
        true
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path)
            || read_header(path, 0x110)
                .map(|data| {
                    std::str::from_utf8(&data[0x100..0x110])
                        .unwrap_or("")
                        .starts_with("SEGA ")
                })
                .unwrap_or(false)
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_MD
    }
    fn name(&self) -> &'static str {
        "Megadrive"
    }
}
