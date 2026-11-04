use super::System;

const CORE_NAME_UW8: &str = "uw8";
pub struct MicroW8System {}

impl System for MicroW8System {
    fn extensions(&self) -> &'static [&'static str] {
        &["uw8", "wasm"]
    }
    fn is_console(&self) -> bool {
        true
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_UW8
    }
    fn name(&self) -> &'static str {
        "MicroW8"
    }
}
