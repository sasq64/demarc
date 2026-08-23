use super::System;

const CORE_NAME_TIC80: &str = "tic80";
pub struct Tic80System {}

impl System for Tic80System {
    fn extensions(&self) -> &'static [&'static str] {
        &["tic", "tic80"]
    }
    fn core_name(&self) -> &'static str {
        CORE_NAME_TIC80
    }
    fn name(&self) -> &'static str {
        "Tic80"
    }
}
