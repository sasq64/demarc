use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use tracing::debug;

use release_io::amiga::{self, AmigaOpts, amiga_system_dir};

use crate::{SysOpts, backend::Backend, libloader, retro_emu, workfile::WorkFile};

use super::System;

pub use amiga::apply_fast;

pub struct AmigaSystem {
    opts: AmigaOpts,
}

impl AmigaSystem {
    pub fn new(args: &SysOpts) -> Self {
        Self {
            opts: AmigaOpts {
                aga: args.aga,
                xmem: args.xmem,
                fast: args.fast,
                silent_drive: args.silent_drive,
                unadf: args.unadf,
            },
        }
    }
}

impl System for AmigaSystem {
    fn extensions(&self) -> &'static [&'static str] {
        amiga::EXTENSIONS
    }

    fn core_name(&self) -> &'static str {
        amiga::CORE_NAME_UAE
    }

    fn name(&self) -> &'static str {
        "Amiga"
    }

    fn default_meta(&self) -> HashMap<&str, &str> {
        amiga::default_meta()
    }

    fn can_load(&self, path: &Path) -> bool {
        amiga::can_load(path)
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        amiga::prepare(file, &self.opts)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let meta = path.get_all_meta();
        let core_name = amiga::core_name(path);
        debug!("Starting {core_name} with meta {meta:?}");
        let core = libloader::get_libretro(core_name).context("Could not load core")?;
        retro_emu::create_core(&core, &amiga_system_dir(), Some(path), meta, false)
    }
}
