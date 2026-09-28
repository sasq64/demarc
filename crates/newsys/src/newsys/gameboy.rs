use super::System;

const CORE_NAME_GAMEBOY: &str = "gambatte";
pub struct GameboySystem {}

impl System for GameboySystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["gb", "gbc"]
    }
    fn is_console(&self) -> bool {
        true
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_GAMEBOY
    }
    fn name(&self) -> &'static str {
        "Gameboy"
    }
}
