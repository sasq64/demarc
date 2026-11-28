use std::path::PathBuf;

use anyhow::Result;
use tracing::warn;

use crate::{
    backend::Backend,
    newsys::walk_dir,
    text_renderer::{TextEmu, TextStyle},
    workfile::WorkFile,
};

use super::System;

const CATEGORY: &str = "ASCII Collection";

fn rank(ext: &str) -> Option<u8> {
    match ext {
        "ans" | "asc" => Some(0),
        "txt" => Some(1),
        "nfo" | "diz" => Some(2),
        _ => None,
    }
}

pub struct AsciiSystem {}

impl System for AsciiSystem {
    fn name(&self) -> &'static str {
        "ASCII"
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        if !file.get_meta_or("category", "").contains(CATEGORY) {
            return Ok(false);
        }
        let mut found: Vec<((u8, usize), PathBuf)> = vec![];
        walk_dir(&file.path.clone(), 0, |path, ext, _| {
            if let Some(r) = rank(ext) {
                found.push(((r, path.components().count()), path.to_owned()));
            }
            Ok(())
        })?;
        found.sort_by_key(|(key, _)| *key);
        match found.into_iter().next() {
            Some((_, path)) => {
                file.path = path;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let platform = path.get_meta_or("platform", "").to_ascii_lowercase();
        if platform.contains("ms-dos") {
            // TODO: PC font and ANSI escape codes.
            warn!("MS-DOS ASCII rendering not done yet, using the Amiga style");
        }
        Ok(Box::new(TextEmu::new(path, TextStyle::Amiga)?))
    }
}

#[cfg(test)]
#[path = "tests/ascii_tests.rs"]
mod tests;
