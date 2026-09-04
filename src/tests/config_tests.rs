use super::*;

/// A "next" pressed while the incoming release is only *waiting* to be
/// shown switches to it instead of loading another one, so the window in
/// which that is true has to be exactly the one where a release is loaded
/// and its fade has not begun.
#[test]
fn a_fade_is_waiting_only_between_the_load_landing_and_the_fade_starting() {
    let mut fade = FadeState::default();
    // Nothing on its way in.
    assert!(!fade.is_waiting(10.0));

    // Handed over, still downloading: there is nothing to switch to.
    fade.incoming = Some(1);
    fade.start = f64::MAX;
    assert!(!fade.is_waiting(10.0));

    // Loaded, sitting out `--cross-fade-delay`.
    fade.start = 12.0;
    assert!(fade.is_waiting(10.0));
    // ...and once the fade is actually running, it is no longer waiting.
    assert!(!fade.is_waiting(12.0));
    assert!(!fade.is_waiting(13.0));

    // A `--cross-wait-sound` hold is a wait however long it lasts.
    fade.start = f64::MAX;
    fade.wait_sound = Some(SoundWait::starting_at(10.0));
    assert!(fade.is_waiting(10.0));
    assert!(fade.is_waiting(10.0 + SOUND_TIMEOUT_SECS));
}

/// The two emulators of a cross-fade need the whole window each, which is
/// exactly what a grid can't give them.
#[test]
fn cross_fade_and_grid_are_mutually_exclusive() {
    assert!(Args::try_parse_from(["demarc", "--cross-fade", "--grid=2x2"]).is_err());
    assert!(Args::try_parse_from(["demarc", "--cross-fade"]).is_ok());
    assert!(Args::try_parse_from(["demarc", "--grid=2x2"]).is_ok());
}

/// A bare `--cross-fade` takes the default length; `--cross-fade=SECS`
/// sets it. The value has to be attached with `=`, or the flag would eat
/// the file that follows it.
#[test]
fn cross_fade_length_is_optional() {
    let args = Args::try_parse_from(["demarc"]).unwrap();
    assert_eq!(args.cross_fade, None);

    let args = Args::try_parse_from(["demarc", "--cross-fade"]).unwrap();
    assert_eq!(args.cross_fade, Some(2.0));

    let args = Args::try_parse_from(["demarc", "--cross-fade=4.5"]).unwrap();
    assert_eq!(args.cross_fade, Some(4.5));

    let args = Args::try_parse_from(["demarc", "--cross-fade", "demo.adf"]).unwrap();
    assert_eq!(args.cross_fade, Some(2.0));
    assert_eq!(args.files, vec![PathBuf::from("demo.adf")]);
}

/// `--cross-wait-sound` needs a fade to hold back, and silences the drive
/// so a loading Amiga can't end the wait by clicking.
#[test]
fn waiting_for_sound_needs_a_fade_and_implies_a_silent_drive() {
    assert!(Args::try_parse_from(["demarc", "--cross-wait-sound"]).is_err());

    let mut args =
        Args::try_parse_from(["demarc", "--cross-fade", "--cross-wait-sound"]).unwrap();
    assert!(!args.silent_drive, "not until the implications are applied");
    args.apply_implications();
    assert!(args.silent_drive);
}

/// The hold ends on the first sound after the grace period, or on its own
/// deadline — whichever comes first.
#[test]
fn a_sound_wait_ends_on_sound_or_on_its_deadline() {
    let wait = SoundWait::starting_at(100.0);

    // Startup static, inside the grace period: ignored.
    assert!(!wait.is_over(100.0, false));
    assert!(!wait.is_over(100.0 + SOUND_GRACE_SECS - 0.01, false));
    // The same sound once the grace period is up ends the hold.
    assert!(wait.is_over(100.0 + SOUND_GRACE_SECS, false));
    // Silence holds it — until the deadline gives up waiting.
    assert!(!wait.is_over(100.0 + SOUND_TIMEOUT_SECS - 0.01, true));
    assert!(wait.is_over(100.0 + SOUND_TIMEOUT_SECS, true));
}

/// Settings mid-fade: emulator 0 on screen, emulator 1 fading in over it.
fn fading(alpha: f32) -> AppSettings {
    AppSettings {
        cross_fade: Some(2.0),
        current_emu: 0,
        fade: FadeState {
            incoming: Some(1),
            start: 0.0,
            alpha,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The outgoing view stays opaque and the incoming one is blended over it,
/// so the two alphas are `1` and the fade position — not `1 - a` and `a`.
#[test]
fn the_incoming_view_alone_fades_in() {
    let s = fading(0.25);
    assert_eq!(s.view_alpha(0), 1.0);
    assert_eq!(s.view_alpha(1), 0.25);

    // Between fades the emulator parked in the background is invisible,
    // which is what makes the render pass skip it entirely.
    let mut s = fading(0.0);
    s.fade.incoming = None;
    assert_eq!(s.view_alpha(0), 1.0);
    assert_eq!(s.view_alpha(1), 0.0);
}

/// Both audio streams are attenuated, on equal-power ramps that hold the
/// summed level roughly constant across the fade.
#[test]
fn both_sides_of_the_audio_fade() {
    let s = fading(0.0);
    assert_eq!(s.audio_gain(0), 1.0);
    assert_eq!(s.audio_gain(1), 0.0);

    let s = fading(1.0);
    assert_eq!(s.audio_gain(0), 0.0);
    assert_eq!(s.audio_gain(1), 1.0);

    // Half way, equal-power means both sides sit at ~0.707, not 0.5.
    let s = fading(0.5);
    assert!((s.audio_gain(0) - 0.5f32.sqrt()).abs() < 1e-6);
    assert!((s.audio_gain(1) - 0.5f32.sqrt()).abs() < 1e-6);
    let power = s.audio_gain(0).powi(2) + s.audio_gain(1).powi(2);
    assert!((power - 1.0).abs() < 1e-6);
}

/// Without `--cross-fade` nothing is ever attenuated or blended, whatever
/// the (unused) fade state happens to say.
#[test]
fn no_cross_fade_leaves_every_view_opaque() {
    let mut s = fading(0.5);
    s.cross_fade = None;
    for i in 0..4 {
        assert_eq!(s.view_alpha(i), 1.0);
        assert_eq!(s.audio_gain(i), 1.0);
    }
}
