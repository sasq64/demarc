use std::{fs, path::Path};

use crate::{Args, newsys::get_ext};

use super::System;

const CORE_NAME_STELLA: &str = "stella";

pub struct Atari2600System {}

impl Atari2600System {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for Atari2600System {
    fn can_load(&self, path: &Path) -> bool {
        let Ok(l) = fs::metadata(path).map(|m| m.len() as usize) else {
            return false;
        };

        let ext = get_ext(path);

        ext == "a26" || (ext == "bin" && l.is_power_of_two() && (2048..=32768).contains(&l))
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_STELLA
    }
    fn name(&self) -> &'static str {
        "Atari 2600"
    }
}
