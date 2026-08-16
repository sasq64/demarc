use std::collections::HashMap;

use super::System;
use super::utils::build_m3u;
use crate::{newsys::walk_dir, workfile::WorkFile};
use anyhow::Result;

const CORE_NAME_CAP32: &str = "cap32";

pub struct AmstradSystem {}

impl System for AmstradSystem {
    fn core_name(&self) -> &'static str {
        CORE_NAME_CAP32
    }

    fn name(&self) -> &'static str {
        "Amstrad"
    }
    fn default_tags(&self) -> HashMap<&str, &str> {
        [("cap32_statusbar", "disabled")].into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        walk_dir(file, 4, |path, ext, _header| {
            println!("{path:?} {ext:?}");
            if ["dsk"].contains(&ext) {
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
