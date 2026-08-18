use std::collections::HashMap;

use super::System;
use crate::{
    newsys::{collect_disk_images, walk_dir},
    workfile::WorkFile,
};
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

    fn default_meta(&self) -> HashMap<&str, &str> {
        [
            //("cap32_model", "6128"),
            //("cap32_ram", "512"),
            ("cap32_statusbar", "disabled"),
        ]
        .into()
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        walk_dir(file, 4, |path, ext, _header| {
            if ["dsk"].contains(&ext) {
                images.push(path.to_owned());
            }
            Ok(())
        })?;

        if !images.is_empty() {
            collect_disk_images(file, &mut images)?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }
}
