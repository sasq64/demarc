use super::System;

/// ZEsarUX, built as a libretro core out of `external/zesarux` — see
/// docs/ZESARUX.md. No buildbot core emulates the Next.
const CORE_NAME_ZESARUX: &str = "zesarux";

pub struct SpectrumNextSystem {}

impl System for SpectrumNextSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["nex", "snx", "trd"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_ZESARUX
    }
    fn name(&self) -> &'static str {
        "ZX Spectrum Next"
    }
}
