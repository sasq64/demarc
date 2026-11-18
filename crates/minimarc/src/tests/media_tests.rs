use super::*;

#[test]
fn headset_and_play_pause_are_taps() {
    assert_eq!(from_keycode(79), Some(Media::Tap));
    assert_eq!(from_keycode(85), Some(Media::Tap));
    assert_eq!(
        from_key(&Key::Named(NamedKey::HeadsetHook)),
        Some(Media::Tap)
    );
    assert_eq!(from_keycode(87), Some(Media::Next));
    assert_eq!(from_keycode(4), None);
}
