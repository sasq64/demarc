use super::*;
use crate::config::SOUND_TIMEOUT_SECS;

/// An app holding just what [`update_cross_fade`] touches — the two
/// emulators and their views — with a clock we advance by hand.
///
/// The fade is set up as if the incoming release had already finished
/// loading at `t = 0`, which is what `run_retro` does when it sees the load
/// land.
fn fade_app(secs: f32) -> App {
    let mut app = App::new();
    app.init_resource::<Time>();
    let mut settings = AppSettings {
        cross_fade: Some(secs),
        ..default()
    };
    settings.fade.incoming = Some(1);
    settings.fade.start = 0.0;
    app.insert_resource(settings);
    for index in 0..CROSS_FADE_EMUS {
        app.world_mut().spawn(Emulator::default());
        app.world_mut().spawn((
            EmuView { index },
            PostProcess {
                source: Handle::default(),
                aspect: 0.0,
                aspect_tweak: 1.0,
                alpha: 1.0,
            },
        ));
    }
    app.add_message::<SetHudText>();
    app.add_systems(Update, update_cross_fade);
    app
}

fn tick(app: &mut App, secs: f32) {
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_secs_f32(secs));
    app.update();
}

/// The views' alphas, by [`EmuView::index`].
fn alphas(app: &mut App) -> Vec<f32> {
    let mut views: Vec<(usize, f32)> = app
        .world_mut()
        .query::<(&EmuView, &PostProcess)>()
        .iter(app.world())
        .map(|(v, pp)| (v.index, pp.alpha))
        .collect();
    views.sort_by_key(|(i, _)| *i);
    views.into_iter().map(|(_, a)| a).collect()
}

#[test]
fn the_incoming_release_fades_in_over_the_outgoing_one() {
    let mut app = fade_app(2.0);

    // A quarter of the way in, only the incoming view is translucent.
    tick(&mut app, 0.5);
    assert_eq!(alphas(&mut app), vec![1.0, 0.25]);
    // Its audio comes up as the outgoing release's goes down.
    let settings = app.world().resource::<AppSettings>();
    assert!(settings.audio_gain(0) > settings.audio_gain(1));
    assert!(settings.audio_gain(1) > 0.0);

    tick(&mut app, 1.0);
    assert_eq!(alphas(&mut app), vec![1.0, 0.75]);
}

/// At the end of the fade the two emulators swap roles: the one that faded
/// in owns the screen, and the one it covered is parked.
#[test]
fn finishing_a_fade_swaps_the_emulators() {
    let mut app = fade_app(2.0);
    tick(&mut app, 2.0);

    let settings = app.world().resource::<AppSettings>();
    assert_eq!(settings.current_emu, 1);
    assert_eq!(settings.fade.incoming, None);
    // The outgoing view is fully transparent, which is what makes the
    // render pass skip it (and its filter chain) entirely.
    assert_eq!(alphas(&mut app), vec![0.0, 1.0]);
    assert!(
        app.world_mut()
            .query::<&Emulator>()
            .iter(app.world())
            .next()
            .unwrap()
            .paused
    );

    // Nothing left to fade: the picture stays put.
    tick(&mut app, 5.0);
    assert_eq!(alphas(&mut app), vec![0.0, 1.0]);
    assert_eq!(app.world().resource::<AppSettings>().current_emu, 1);
}

/// A hand-over in flight takes the playlist away from every view, so the
/// idle timeout on the release still on screen — which goes on being idle
/// for the whole download plus fade — cannot ask for another one and
/// restart the fade.
#[test]
fn nobody_drives_the_playlist_while_a_fade_is_pending() {
    let mut settings = AppSettings {
        cross_fade: Some(2.0),
        ..default()
    };
    assert!(drives_playlist(&settings, 0));
    assert!(!drives_playlist(&settings, 1));

    settings.fade.incoming = Some(1);
    assert!(!drives_playlist(&settings, 0));
    assert!(!drives_playlist(&settings, 1));

    // Without a fade the single view always owns the playlist.
    settings.cross_fade = None;
    assert!(drives_playlist(&settings, 0));
}

/// `--cross-wait-sound` holds the fade until the incoming release is
/// audible. The emulators here have no backend at all, which reads as
/// silent, so nothing but the hold's own deadline can start this fade.
#[test]
fn a_sound_wait_holds_the_fade_until_something_ends_it() {
    let mut app = fade_app(2.0);
    {
        let mut settings = app.world_mut().resource_mut::<AppSettings>();
        settings.cross_wait_sound = true;
        settings.cross_fade_delay = 1.0;
        settings.fade.start = f64::MAX;
        settings.fade.wait_sound = Some(SoundWait::starting_at(0.0));
        settings.fade.pending_info = Some("Some Demo".into());
    }

    // Well past the grace period, still silent: nothing has moved.
    tick(&mut app, 5.0);
    assert_eq!(alphas(&mut app), vec![1.0, 0.0]);
    assert!(
        app.world()
            .resource::<AppSettings>()
            .fade
            .wait_sound
            .is_some()
    );

    // The hold's deadline is the backstop that keeps a silent release from
    // parking the playlist for good.
    tick(&mut app, SOUND_TIMEOUT_SECS as f32);
    let settings = app.world().resource::<AppSettings>();
    assert!(settings.fade.wait_sound.is_none());
    // ...and the info text it was sitting on goes out with it.
    assert!(settings.fade.pending_info.is_none());
    // `--cross-fade-delay` runs from there, so the fade is still at zero a
    // moment later and half way through a second after that.
    tick(&mut app, 0.9);
    assert_eq!(alphas(&mut app), vec![1.0, 0.0]);
    tick(&mut app, 1.1);
    assert_eq!(alphas(&mut app), vec![1.0, 0.5]);
}

/// Nothing fades while the incoming release is still loading — `run_retro`
/// only dates the fade once the core is actually running.
#[test]
fn a_fade_waits_for_the_release_to_load() {
    let mut app = fade_app(2.0);
    app.world_mut().resource_mut::<AppSettings>().fade.start = f64::MAX;

    tick(&mut app, 10.0);
    assert_eq!(alphas(&mut app), vec![1.0, 0.0]);
    assert_eq!(app.world().resource::<AppSettings>().current_emu, 0);
}
