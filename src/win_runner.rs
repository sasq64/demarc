//! Windows releases on Windows: the demo is simply started.
//!
//! There is no emulation and no capture here — the demo owns the screen while
//! it runs, and demarc only holds its process. The setup dialog is still
//! answered by `demarc-autodlg.exe`, which is also what says when the demo has
//! ended; see [`crate::wine::native_command`] for the command, and
//! [`crate::newsys::windows`], which builds the gamescope-captured version of
//! the same thing on Linux.

use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::backend::Backend;
use crate::wine::native_command;
use crate::workfile::WorkFile;

/// The frame handed to the frontend. The demo draws to the desktop rather than
/// to us, so there is nothing to show and no reason for it to be big.
const FRAME_W: usize = 64;
const FRAME_H: usize = 36;

/// What the frontend is paced at while a demo runs.
const FRAME_RATE: f64 = 60.0;

pub struct WinRunner {
    child: Child,
    frame: Vec<u32>,
    aspect: f32,
    ended: bool,
}

impl WinRunner {
    pub fn new(file: &WorkFile) -> Result<Self> {
        let cmd = native_command(&file.path, &file.get_all_meta())?;
        let (program, args) = cmd
            .argv
            .split_first()
            .context("Nothing to run this release with")?;
        info!("Running {program} {args:?}");
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Where the demo's own data files are, for the case where there is no
        // driver to start it from its own directory.
        if let Some(dir) = file.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            command.current_dir(dir);
        }
        let child = command
            .spawn()
            .with_context(|| format!("Could not start {program}"))?;
        Ok(Self {
            child,
            frame: vec![0; FRAME_W * FRAME_H],
            aspect: cmd.width as f32 / cmd.height.max(1) as f32,
            ended: false,
        })
    }
}

impl Drop for WinRunner {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        // The driver started the demo, so killing the driver alone would leave
        // the demo on the screen with nothing holding it.
        let pid = self.child.id().to_string();
        let killed = Command::new("taskkill")
            .args(["/T", "/F", "/PID", pid.as_str()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if killed.is_err() {
            warn!("Could not stop the demo: {killed:?}");
        }
        let _ = self.child.kill();
    }
}

impl Backend for WinRunner {
    fn run(&mut self) -> bool {
        if self.ended {
            return false;
        }
        // An error here is a child that is no longer ours to wait for, which
        // says the same thing an exit status does.
        if let Ok(None) = self.child.try_wait() {
            return true;
        }
        info!("The demo has ended");
        self.ended = true;
        false
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        f(FRAME_W, FRAME_H, &self.frame);
    }

    fn with_audio(&mut self, _f: &mut dyn FnMut(&[i16])) {}

    fn get_frame_size(&self) -> (usize, usize) {
        (FRAME_W, FRAME_H)
    }

    /// The shape of the session, so the black the frontend draws is the shape
    /// the demo was started at.
    fn aspect_ratio(&self) -> f32 {
        self.aspect
    }

    fn sample_rate(&self) -> f64 {
        0.0
    }

    fn fps(&self) -> f64 {
        FRAME_RATE
    }

    /// How the frontend hears that the demo is over and moves on, the same way
    /// a tune that has played out says so.
    fn is_idle(&self) -> bool {
        self.ended
    }

    // The demo has the keyboard and the mouse itself; nothing here is routed to
    // it, and there is no core to reset or swap disks in.
    fn set_disk(&mut self, _no: u32) {}
    fn get_number_of_disks(&mut self) -> u32 {
        1
    }
    fn reset(&mut self) {}
    fn press_key(&mut self, _code: u32, _down: bool, _mods: u16) {}
    fn add_mouse_motion(&mut self, _dx: f32, _dy: f32) {}
    fn set_mouse_buttons(&mut self, _left: bool, _right: bool, _middle: bool) {}
    fn set_joypad(&mut self, _port: u32, _id: u32, _down: bool) {}
    fn skip_frames(&mut self, _frames: u32) {}
}
