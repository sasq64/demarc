//! The interface the frontend uses to drive an emulated view, and the shared
//! frame representation that goes with it.
//!
//! Deliberately free of any libretro dependency: the libretro cores in
//! [`crate::retro_emu`] are only one implementation, alongside the image, music,
//! Flash and Wine backends.

/// How much of the user's attention a view has, handed to the backend by
/// [`Backend::focus`].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum ViewFocus {
    /// Not on screen at all: another view is maximized over this one.
    Invisible,
    /// Drawn as one tile of the grid, but not the selected view.
    Visible,
    /// The selected view — exactly one emulator has this at a time, whether it
    /// is maximized or one tile among many.
    #[default]
    Focus,
}

/// Bit in the mask returned by [`Backend::state`]: the backend is fast-forwarding
/// through the frames asked for by [`Backend::skip_frames`] and has not caught up
/// yet. Cleared on the frame the skip runs out.
pub const STATE_SKIPPING: u64 = 1 << 0;

/// Abstract interface over a libretro emulator core.
pub trait Backend {
    fn set_disk(&mut self, no: u32);
    /// Takes `&mut self` because the libretro implementation calls into the
    /// core, which may issue environment callbacks while it does.
    fn get_number_of_disks(&mut self) -> u32;
    /// Step the emulator by one presented frame
    fn run(&mut self) -> bool;

    fn reset(&mut self);
    fn press_key(&mut self, code: u32, down: bool, mods: u16);
    fn add_mouse_motion(&mut self, dx: f32, dy: f32);
    /// Set the absolute pointer position in normalized frame coordinates
    /// (`0.0..=1.0`, origin top-left). Cores driven by relative mouse motion
    /// (libretro) ignore this; Flash needs it so Ruffle's internal cursor tracks
    /// the visible OS cursor for hit-testing buttons.
    fn set_mouse_position(&mut self, _x: f32, _y: f32) {}
    fn set_mouse_buttons(&mut self, left: bool, right: bool, middle: bool);
    fn set_joypad(&mut self, port: u32, id: u32, down: bool);
    fn with_frame(&self, f: &mut dyn FnMut(usize, usize, &[u32]));
    fn with_audio(&mut self, f: &mut dyn FnMut(&[i16]));
    fn get_frame_size(&self) -> (usize, usize);
    fn aspect_ratio(&self) -> f32;
    fn sample_rate(&self) -> f64;
    fn fps(&self) -> f64;
    // fn unload(&mut self);
    fn skip_frames(&mut self, frames: u32);
    /// Total number of emulated frames the core has stepped so far. Used by the
    /// `--speed-test` benchmark to measure throughput. Defaults to 0 for cores
    /// that don't track it.
    fn frames_stepped(&self) -> u64 {
        0
    }
    /// Bitmask of what the backend is doing right now, for the frontend to
    /// reflect in the UI — see the `STATE_*` constants. Read every displayed
    /// frame, so it must be cheap (an atomic load for the threaded core, which
    /// is the only backend that has anything to report). Backends that don't
    /// track it report nothing.
    fn state(&self) -> u64 {
        0
    }

    /// A value that changes whenever [`with_frame`](Self::with_frame) would hand
    /// back different pixels than it did last time.
    ///
    /// The frontend re-uploads the emulator's texture only when this moves, so a
    /// backend that leaves it constant is never redrawn — which is why there is
    /// no default implementation. Any monotonic counter or content hash will do;
    /// it only has to differ, not to increase.
    fn frame_hash(&self) -> u64;
    fn is_idle(&self) -> bool {
        false
    }

    /// Whether the backend is producing no sound right now: the audio half of
    /// [`is_idle`](Self::is_idle), on its own. `--cross-wait-sound` holds a
    /// cross-fade back until the release coming in is actually audible, which
    /// is a question about the sound alone — a demo on its loading screen is
    /// silent but far from idle, and a still image is the other way round.
    ///
    /// The default is `false`, i.e. "assume it is making sound". A backend that
    /// doesn't track its audio cannot answer, and a caller waiting for sound
    /// must not end up waiting on it forever.
    fn is_silent(&self) -> bool {
        false
    }

    /// Tell the backend how much the user is looking at it — see [`ViewFocus`].
    /// A backend that runs just as well unwatched ignores it; the music backend
    /// uses it to stop rendering audio nobody is listening to.
    fn focus(&mut self, _focus: ViewFocus) {}

    /// Schedule key presses to be played back into the core, as
    /// `(frame, keycode)` pairs. The frame is relative to now — `0` means the
    /// next stepped frame — and each key is released two frames after it is
    /// pressed. Used to feed a core its "startup keys".
    fn send_keys(&mut self, _keys: &[(u32, u32)]) {}

    fn get_info(&self) -> Option<String> {
        None
    }
}

/// Reinterpret a slice of packed RGBA pixels as the raw bytes the GPU texture
/// upload (and PNG encoder) expect. Each `u32` holds one pixel with its bytes
/// already in `[r, g, b, a]` memory order (see the LUTs / `video_refresh`), so
/// this is a plain, always-sound width-narrowing view.
pub fn frame_bytes(pixels: &[u32]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(pixels.as_ptr() as *const u8, std::mem::size_of_val(pixels))
    }
}
