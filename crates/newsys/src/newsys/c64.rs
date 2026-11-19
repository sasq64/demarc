use anyhow::{Context, Result};

use crate::{SysOpts, backend::Backend, libloader, retro_emu, system_dir, workfile::WorkFile};

use super::System;

const CORE_NAME_VICE_64SC: &str = "vice_x64sc";
// const CORE_NAME_VICE_64: &str = "vice_x64";
// const CORE_NAME_VICE_DTV: &str = "vice_x64dtv";
// const CORE_NAME_VICE_128: &str = "vice_x128";
// const CORE_NAME_VICE_C16: &str = "vice_xplus4";
// const CORE_NAME_VICE_VIC20: &str = "vice_xvic";

pub struct C64System {
    reu: bool,
}

impl C64System {
    pub fn new(args: &SysOpts) -> Self {
        Self { reu: args.reu }
    }
}

impl System for C64System {
    fn core_name(&self) -> &'static str {
        CORE_NAME_VICE_64SC
    }

    fn name(&self) -> &'static str {
        "C64"
    }

    fn load(&self, file: &mut WorkFile) -> Result<bool> {
        release_io::c64::prepare(file, self.reu)
    }

    fn create(&self, path: &WorkFile) -> Result<Box<dyn Backend + Send + Sync>> {
        let core = libloader::get_libretro(self.core_name()).context("Could not load core")?;
        let mut core =
            retro_emu::create_core(&core, system_dir(), Some(path), path.get_all_meta(), false)?;
        core.send_events(&release_io::c64::boot_events(path));
        Ok(core)
    }
}
