use super::System;

const CORE_NAME_GAMEBOY: &str = "gambatte";
pub struct GameboySystem {}

impl System for GameboySystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["gb", "gbc"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_GAMEBOY
    }
}
