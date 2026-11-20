use super::*;
use crate::tests::{Harness, key};

#[test]
fn shows_each_line_and_closes_on_enter() {
    let mut h = Harness::new();
    let mut mb = MessageBox::default();
    mb.open("Demarc\n1.0");
    // A new Area paints nothing on its first frame.
    h.frame(vec![], |ctx| mb.show(ctx));
    h.frame(vec![], |ctx| mb.show(ctx));
    assert!(h.has("Demarc") && h.has("1.0") && h.has("OK"));
    h.frame(vec![key(egui::Key::Enter)], |ctx| mb.show(ctx));
    assert!(!mb.is_open());
}

#[test]
fn closes_on_ok() {
    let mut h = Harness::new();
    let mut mb = MessageBox::default();
    mb.open("Hello");
    h.frame(vec![], |ctx| mb.show(ctx));
    h.frame(vec![], |ctx| mb.show(ctx));
    h.click_text("OK", |ctx| mb.show(ctx));
    assert!(!mb.is_open());
}
