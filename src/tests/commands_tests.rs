use super::*;

#[test]
fn pad_modifier_picks_the_combined_mapping() {
    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::West);
    assert_eq!(check_pad(&pad), Some(Cmd::OpenFile));

    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::LeftTrigger2);
    assert_eq!(check_pad(&pad), None);
    pad.clear();
    pad.press(GamepadButton::West);
    assert_eq!(check_pad(&pad), Some(Cmd::ChangeScale));

    // A button with no combined mapping does nothing while the modifier is held.
    pad.clear();
    pad.press(GamepadButton::Start);
    assert_eq!(check_pad(&pad), None);
}

#[test]
fn pad_select_toggles_the_keyboard() {
    let mut pad = ButtonInput::<GamepadButton>::default();
    pad.press(GamepadButton::Select);
    assert_eq!(check_pad(&pad), Some(Cmd::OpenKeyboard));
}

#[test]
fn command_list_shows_pad_buttons() {
    let pad = command_list(true);
    assert_eq!(pad.len(), PAD_HOTKEYS.len());
    assert_eq!(
        pad[0],
        (" \u{f0c32}      Open file menu ".to_string(), Cmd::OpenFile)
    );
    assert!(pad.iter().any(|(line, _)| line.starts_with(" L2+R1 ")));
    assert_eq!(command_list(false).len(), HOTKEYS.len());
}

#[test]
fn stick_moves_the_cursor_outside_the_deadzone() {
    assert_eq!(stick_velocity(Vec2::new(0.05, -0.05)), Vec2::ZERO);
    assert_eq!(
        stick_velocity(Vec2::new(1.0, 0.0)),
        Vec2::new(PAD_MOUSE_SPEED, 0.0)
    );
    // Stick up is screen up.
    assert!(stick_velocity(Vec2::new(0.0, 0.5)).y < 0.0);
}
