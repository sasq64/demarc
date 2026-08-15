use crate::{image_emu::ImageEmu, newsys::walk_dir, retro_emu::Backend, workfile::WorkFile};
use anyhow::Result;

use super::System;

pub struct ImageSystem {}

impl System for ImageSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["lbm", "iff"]
    }
    fn name(&self) -> &'static str {
        "Images"
    }
    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        let mut images = vec![];
        walk_dir(&file.path.clone(), 12, |path, ext, header| {
            println!("{path:?} {ext:?}");
            if ["iff", "ilbm", "png", "bmp", "jpg", "jpeg", "gif"].contains(&ext)
                || &header[0..4] == b"FORM" && &header[8..12] == b"ILBM"
            {
                images.push(path.to_owned());
            }
            Ok(())
        })?;
        if images.is_empty() {
            return Ok(false);
        }

        // Prefer IFF/ILBM images over other formats
        images.sort_by_key(|path| {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            match ext.as_str() {
                "iff" | "ilbm" | "lbm" => 0,
                _ => 1,
            }
        });
        file.path = images[0].clone();
        Ok(true)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        println!("PATH {path:?}");
        let backend = Box::new(ImageEmu::new(&path)?);
        Ok(backend)
    }
}
