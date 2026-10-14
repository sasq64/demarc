//! Per-frame pacing stats written as CSV by `--frame-log`; plot them with
//! `scripts/plot_frame_log.py`.

use std::fs::File;
use std::io::{LineWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

static LOG: Mutex<Option<LineWriter<File>>> = Mutex::new(None);
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

pub struct FrameStats {
    pub emu: usize,
    pub time: f64,
    pub delta: f64,
    pub fill: usize,
    pub queued: usize,
    pub buf_min: usize,
    pub buf_max: usize,
    pub fps: f64,
    pub cpu: f32,
}

pub fn open(path: &Path) -> std::io::Result<()> {
    let mut file = LineWriter::new(File::create(path)?);
    writeln!(file, "time,emu,delta,fill,queued,buf_min,buf_max,fps,cpu")?;
    *LOG.lock().unwrap() = Some(file);
    Ok(())
}

/// A fresh id for the `emu` column, so grid views can be told apart.
pub fn next_id() -> usize {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn log(s: &FrameStats) {
    if let Some(file) = LOG.lock().unwrap().as_mut() {
        let _ = writeln!(
            file,
            "{:.6},{},{:.6},{},{},{},{},{:.4},{:.4}",
            s.time, s.emu, s.delta, s.fill, s.queued, s.buf_min, s.buf_max, s.fps, s.cpu
        );
    }
}
