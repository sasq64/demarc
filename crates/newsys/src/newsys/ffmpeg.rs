use std::path::Path;

use anyhow::Result;

use super::System;
use crate::{backend::Backend, ffmpeg_emu, ffmpeg_emu::FfmpegEmu, workfile::WorkFile};

const EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mkv", "webm", "avi", "mov", "mpg", "mpeg", "wmv", "flv", "ogv",
];

pub struct FfmpegSystem {}

impl System for FfmpegSystem {
    fn extensions(&self) -> &'static [&'static str] {
        EXTENSIONS
    }

    fn name(&self) -> &'static str {
        "FFmpeg"
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path) && ffmpeg_emu::is_available()
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        Ok(Box::new(FfmpegEmu::new(path)?))
    }
}

#[cfg(test)]
#[path = "tests/ffmpeg_tests.rs"]
mod tests;
