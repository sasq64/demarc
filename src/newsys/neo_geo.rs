use super::System;

const CORE_NAME_GEOLITH: &str = "geolith";
pub struct NeoGeoSystem {}

impl System for NeoGeoSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["neo", "cue"]
    }
    fn is_console(&self) -> bool {
        true
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_GEOLITH
    }
    fn name(&self) -> &'static str {
        "NeoGeo"
    }
}
