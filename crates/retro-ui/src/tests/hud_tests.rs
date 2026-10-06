use super::*;
use crate::tests::Harness;

const SECOND: Duration = Duration::from_secs(1);

/// Two frames, because an [`egui::Area`] paints nothing on its first.
fn shown(h: &mut Harness, hud: &Hud, now: f32, bytes: u64) {
    h.frame(vec![], |ctx| hud.show(ctx, now, bytes));
    h.frame(vec![], |ctx| hud.show(ctx, now, bytes));
}

/// A text is up from its delay until its duration has run out.
#[test]
fn a_text_shows_for_its_duration() {
    let mut h = Harness::new();
    let mut hud = Hud::default();
    hud.set_text(HudLocation::TopLeft, "Hello", 10.0, SECOND, SECOND * 2);

    shown(&mut h, &hud, 10.5, 0);
    assert!(!h.has("Hello"), "still inside the delay");
    shown(&mut h, &hud, 11.5, 0);
    assert!(h.has("Hello"));
    // Past the end, and long enough for the fade to finish.
    for _ in 0..10 {
        shown(&mut h, &hud, 13.5, 0);
    }
    assert!(!h.has("Hello"));
}

/// Every corner draws its own text at the same time.
#[test]
fn each_location_has_its_own_text() {
    let mut h = Harness::new();
    let mut hud = Hud::default();
    let all = [
        (HudLocation::TopLeft, "tl"),
        (HudLocation::TopRight, "tr"),
        (HudLocation::BottomLeft, "bl"),
        (HudLocation::InfoText, "info"),
        (HudLocation::Error, "error"),
    ];
    for (location, text) in all {
        hud.set_text(location, text, 0.0, Duration::ZERO, SECOND);
    }
    shown(&mut h, &hud, 0.5, 0);
    for (_, text) in all {
        assert!(h.has(text), "{text} missing");
    }
    let screen = crate::tests::SCREEN;
    assert!(h.find("tl").center().y < screen.y / 2.0);
    assert!(h.find("tr").center().x > screen.x / 2.0);
    assert!(h.find("bl").center().y > screen.y / 2.0);
}

/// An empty text ends what is showing rather than replacing it, and is
/// nothing at all for a corner that shows nothing.
#[test]
fn an_empty_text_retires_the_current_one() {
    let mut h = Harness::new();
    let mut hud = Hud::default();
    hud.set_text(HudLocation::InfoText, "", 0.0, Duration::ZERO, SECOND);
    hud.set_text(
        HudLocation::InfoText,
        "Hello",
        0.0,
        Duration::ZERO,
        SECOND * 60,
    );
    shown(&mut h, &hud, 1.0, 0);
    assert!(h.has("Hello"));

    hud.set_text(HudLocation::InfoText, "", 2.0, Duration::ZERO, SECOND);
    for _ in 0..10 {
        shown(&mut h, &hud, 3.0, 0);
    }
    assert!(!h.has("Hello"));
}

/// The download counter reads in KB below a megabyte, in MB from there, and
/// is gone while nothing is downloading.
#[test]
fn downloads_are_counted_in_the_corner() {
    let mut h = Harness::new();
    let hud = Hud::default();
    shown(&mut h, &hud, 0.0, 0);
    assert!(!h.has("\u{f409} 2 KB"));
    for _ in 0..15 {
        shown(&mut h, &hud, 0.0, 1500);
    }
    assert!(h.has("\u{f409} 2 KB"));
    shown(&mut h, &hud, 0.0, 3 * 1024 * 1024 / 2);
    assert!(h.has("\u{f409} 1.5 MB"));
}
