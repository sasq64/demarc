use std::collections::HashMap;

use super::System;
use super::utils::build_m3u;
use crate::{Args, newsys::walk_dir, workfile::WorkFile};
use anyhow::Result;

const CORE_NAME_ATARIXL: &str = "atari800";

pub struct AtariXlSystem {}

impl AtariXlSystem {
    pub fn new(_args: &Args) -> Self {
        Self {}
    }
}

impl System for AtariXlSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_ATARIXL
    }

    fn name(&self) -> &'static str {
        "Atari XL"
    }
    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            ("atari800_ntscpal", "PAL"),
            ("atari800_system", "Modern XL/XE(1088K)"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        walk_dir(file, 4, |path, ext, _header| {
            println!("{path:?} {ext:?}");
            if ["atr", "xex", "atx"].contains(&ext) {
                images.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            let m3u = build_m3u(&images, file)?;
            file.path = m3u;
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
