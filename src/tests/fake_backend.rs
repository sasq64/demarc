use std::sync::{Arc, Mutex, MutexGuard};

use crate::backend::{Backend, VideoFrame, ViewFocus};

/// What a [`FakeBackend`] was told, and what it answers with.
pub struct FakeState {
    pub runs: u32,
    pub resets: u32,
    pub disk: u32,
    pub disks: u32,
    pub skipped: u32,
    pub keys: Vec<(u32, bool, u16)>,
    pub joypad: Vec<(u32, u32, bool)>,
    pub motion: Vec<(f32, f32)>,
    pub pointer: Vec<(f32, f32)>,
    pub buttons: Vec<(bool, bool, bool)>,
    pub focus: Vec<ViewFocus>,

    pub frame: VideoFrame,
    pub used: Option<(usize, usize)>,
    pub audio: Vec<i16>,
    pub fps: f64,
    pub aspect: f32,
    pub idle: bool,
    pub state: u64,
    pub info: Option<String>,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            runs: 0,
            resets: 0,
            disk: 0,
            disks: 1,
            skipped: 0,
            keys: Vec::new(),
            joypad: Vec::new(),
            motion: Vec::new(),
            pointer: Vec::new(),
            buttons: Vec::new(),
            focus: Vec::new(),
            frame: VideoFrame::default(),
            used: None,
            audio: Vec::new(),
            fps: 50.0,
            aspect: 4.0 / 3.0,
            idle: false,
            state: 0,
            info: None,
        }
    }
}

/// A core that emulates nothing: it records what the frontend asks of it and
/// answers from [`FakeState`], which the test keeps a handle on.
#[derive(Clone, Default)]
pub struct FakeBackend(Arc<Mutex<FakeState>>);

impl FakeBackend {
    pub fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.0.lock().unwrap()
    }

    pub fn boxed(&self) -> Option<Box<dyn Backend + Send + Sync>> {
        Some(Box::new(self.clone()))
    }
}

/// A `width`x`height` frame with every pixel set to `pixel`.
pub fn solid_frame(width: usize, height: usize, pixel: u32) -> VideoFrame {
    VideoFrame {
        width,
        height,
        pixels: Arc::new(vec![pixel; width * height]),
    }
}

impl Backend for FakeBackend {
    fn set_disk(&mut self, no: u32) {
        self.lock().disk = no;
    }

    fn get_number_of_disks(&mut self) -> u32 {
        self.lock().disks
    }

    fn run(&mut self) -> bool {
        self.lock().runs += 1;
        true
    }

    fn reset(&mut self) {
        self.lock().resets += 1;
    }

    fn press_key(&mut self, code: u32, down: bool, mods: u16) {
        self.lock().keys.push((code, down, mods));
    }

    fn add_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.lock().motion.push((dx, dy));
    }

    fn set_mouse_position(&mut self, x: f32, y: f32) {
        self.lock().pointer.push((x, y));
    }

    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool) {
        self.lock().buttons.push((left, right, middle));
    }

    fn set_joypad(&mut self, port: u32, id: u32, down: bool) {
        self.lock().joypad.push((port, id, down));
    }

    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32])) {
        let frame = self.frame();
        f(frame.width, frame.height, &frame.pixels);
    }

    fn frame(&self) -> VideoFrame {
        self.lock().frame.clone()
    }

    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16])) {
        f(&self.lock().audio);
    }

    fn get_frame_size(&self) -> (usize, usize) {
        let state = self.lock();
        (state.frame.width, state.frame.height)
    }

    fn get_used_frame_size(&self) -> (usize, usize) {
        let used = self.lock().used;
        used.unwrap_or_else(|| self.get_frame_size())
    }

    fn aspect_ratio(&self) -> f32 {
        self.lock().aspect
    }

    fn sample_rate(&self) -> f64 {
        44100.0
    }

    fn fps(&self) -> f64 {
        self.lock().fps
    }

    fn skip_frames(&mut self, frames: u32) {
        self.lock().skipped += frames;
    }

    fn state(&self) -> u64 {
        self.lock().state
    }

    fn is_idle(&self) -> bool {
        self.lock().idle
    }

    fn focus(&mut self, focus: ViewFocus) {
        self.lock().focus.push(focus);
    }

    fn get_info(&self) -> Option<String> {
        self.lock().info.clone()
    }
}
