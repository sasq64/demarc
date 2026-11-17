//! Headset and media buttons. In the foreground they arrive as winit keys; in
//! the background `MiniActivity`'s MediaSession hands them over through JNI.

use std::sync::Mutex;

use winit::keyboard::{Key, NamedKey};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Media {
    /// Play/pause or the headset button: one tap toggles pause, two skip.
    Tap,
    /// Play or pause. A Bluetooth headset sends pause every time, since the
    /// session always says it is playing.
    Toggle,
    Next,
    Prev,
}

pub fn from_key(key: &Key) -> Option<Media> {
    match key {
        Key::Named(NamedKey::MediaPlayPause | NamedKey::HeadsetHook) => Some(Media::Tap),
        Key::Named(NamedKey::MediaPlay | NamedKey::MediaPause) => Some(Media::Toggle),
        Key::Named(NamedKey::MediaTrackNext) => Some(Media::Next),
        Key::Named(NamedKey::MediaTrackPrevious) => Some(Media::Prev),
        _ => None,
    }
}

/// An Android `KeyEvent` keycode.
pub fn from_keycode(code: i32) -> Option<Media> {
    match code {
        79 | 85 => Some(Media::Tap),
        126 | 127 => Some(Media::Toggle),
        87 => Some(Media::Next),
        88 => Some(Media::Prev),
        _ => None,
    }
}

static PENDING: Mutex<Vec<Media>> = Mutex::new(Vec::new());

/// The buttons pressed since the last call.
pub fn take() -> Vec<Media> {
    std::mem::take(&mut *PENDING.lock().unwrap())
}

/// `MiniActivity.nativeMediaKey(int)`.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_demarc_c64_MiniActivity_nativeMediaKey(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    code: i32,
) -> bool {
    let Some(media) = from_keycode(code) else {
        return false;
    };
    PENDING.lock().unwrap().push(media);
    true
}

#[cfg(test)]
#[path = "tests/media_tests.rs"]
mod tests;
