use std::collections::HashMap;

use super::System;

const CORE_NAME_ZX: &str = "fuse";
pub struct SinclairSystem {}

impl System for SinclairSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["tap", "scl", "trd"]
    }
    fn default_tags(&self) -> HashMap<&str, &str> {
        [("fuse_machine", "Spectrum 128K")].into()
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_ZX
    }
    fn name(&self) -> &'static str {
        "ZX Spectrum"
    }
}
