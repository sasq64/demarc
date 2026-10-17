use super::*;
use bevy::ecs::system::RunSystemOnce;


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

#[test]
fn every_bound_command_has_a_name() {
    for cmd in HOTKEYS.iter().map(|m| m.cmd).chain(PAD_HOTKEYS.iter().map(|m| m.cmd)) {
        assert_eq!(Cmd::from_name(&format!("{cmd:?}")), Some(cmd));
    }
    assert_eq!(Cmd::from_name("NoSuchCommand"), None);
}

#[test]
fn shift_picks_the_shifted_hotkey() {
    let mut input = ButtonInput::<KeyCode>::default();
    assert_eq!(check_hotkey(&input), None);
    input.press(KeyCode::KeyO);
    assert_eq!(check_hotkey(&input), Some(Cmd::OpenFile));
    input.release(KeyCode::KeyO);
    input.clear();
    input.press(KeyCode::ShiftRight);
    assert_eq!(check_hotkey(&input), None);
    input.press(KeyCode::KeyO);
    assert_eq!(check_hotkey(&input), Some(Cmd::StartOther));
    // A key with only an unshifted binding does nothing with shift held.
    input.clear();
    input.press(KeyCode::KeyC);
    assert_eq!(check_hotkey(&input), None);
}

#[test]
fn hotkey_labels_use_keyboard_glyphs() {
    assert_eq!(KeyMapping::new(KeyCode::KeyA, Cmd::ToggleAll).label(), "\u{f0b08}");
    assert_eq!(KeyMapping::new(KeyCode::KeyZ, Cmd::ShaderDialog).label(), "\u{f0b21}");
    assert_eq!(
        KeyMapping::shifted(KeyCode::Tab, Cmd::PrevEmu).label(),
        "\u{f0636} + \u{f0312}"
    );
}

/// What the input and command systems read, headless and with no emulators.
fn world() -> (World, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut world = World::new();
    world.init_resource::<Assets<Image>>();
    world.init_resource::<AppSettings>();
    world.init_resource::<RenderSettings>();
    world.init_resource::<DemarcSettings>();
    world.init_resource::<UiState>();
    world.init_resource::<Time>();
    world.init_resource::<ButtonInput<KeyCode>>();
    world.init_resource::<Messages<CmdMessage>>();
    world.init_resource::<Messages<SetHudText>>();
    world.init_resource::<Messages<ShowFuzzyList>>();
    world.init_resource::<Messages<ShowSettings<DemarcSettings>>>();
    world.init_resource::<Messages<ShowShaderDialog>>();
    world.init_resource::<Messages<bevy::input::keyboard::KeyboardInput>>();
    world.insert_resource(Navigator::new());
    world.insert_resource(Playlists::load(dir.path()));
    (world, dir)
}

fn sent(world: &mut World) -> Vec<Cmd> {
    world
        .resource_mut::<Messages<CmdMessage>>()
        .drain()
        .map(|m| m.0)
        .collect()
}

fn run_cmds(world: &mut World, cmds: &[Cmd]) {
    for &cmd in cmds {
        world.write_message(CmdMessage(cmd));
    }
    world.run_system_once(handle_cmd).unwrap();
    world.resource_mut::<Messages<CmdMessage>>().clear();
}

fn spawn(world: &mut World, index: usize) -> Entity {
    crate::frontend::spawn_emulator(world, false, None, false, index, None)
}

#[test]
fn hotkeys_need_the_modifier_and_no_modal() {
    let (mut world, _dir) = world();
    let mut input = world.resource_mut::<ButtonInput<KeyCode>>();
    input.press(KeyCode::KeyN);
    world.run_system_once(handle_hotkey).unwrap();
    assert!(sent(&mut world).is_empty());

    world.resource_mut::<ButtonInput<KeyCode>>().press(KeyCode::ControlRight);
    world.run_system_once(handle_hotkey).unwrap();
    assert_eq!(sent(&mut world), [Cmd::NextFile]);

    world.resource_mut::<UiState>().modal = true;
    world.run_system_once(handle_hotkey).unwrap();
    assert!(sent(&mut world).is_empty());
}

#[test]
fn modal_ui_only_lets_the_keyboard_pad_command_through() {
    let (mut world, _dir) = world();
    let mut pad = Gamepad::default();
    pad.digital_mut().press(GamepadButton::West);
    pad.digital_mut().press(GamepadButton::Select);
    world.spawn(pad);
    world.resource_mut::<UiState>().modal = true;
    world.run_system_once(handle_gamepad).unwrap();
    // `check_pad` takes the first match, so only OpenFile would fire unmodal.
    assert!(sent(&mut world).is_empty());

    let mut pad = Gamepad::default();
    pad.digital_mut().press(GamepadButton::Select);
    world.spawn(pad);
    world.run_system_once(handle_gamepad).unwrap();
    assert_eq!(sent(&mut world), [Cmd::OpenKeyboard]);
}

#[test]
fn pad_navigation_types_keys_only_in_a_dialog_but_always_releases() {
    use bevy::input::keyboard::KeyboardInput;
    let (mut world, _dir) = world();
    world.spawn((Window::default(), PrimaryWindow));
    let pad = world.spawn(Gamepad::default()).id();
    let press = |world: &mut World, down: bool| {
        let mut pad = world.get_mut::<Gamepad>(pad).unwrap();
        let digital = pad.digital_mut();
        digital.clear();
        if down {
            digital.press(GamepadButton::South);
        } else {
            digital.release(GamepadButton::South);
        }
    };
    let typed = |world: &mut World| -> Vec<(KeyCode, ButtonState)> {
        world.run_system_once(gamepad_navigation).unwrap();
        world
            .resource_mut::<Messages<KeyboardInput>>()
            .drain()
            .map(|k| (k.key_code, k.state))
            .collect()
    };

    press(&mut world, true);
    assert!(typed(&mut world).is_empty());
    press(&mut world, false);
    assert_eq!(typed(&mut world), [(KeyCode::Enter, ButtonState::Released)]);

    world.resource_mut::<UiState>().modal = true;
    press(&mut world, true);
    assert_eq!(typed(&mut world), [(KeyCode::Enter, ButtonState::Pressed)]);
}

#[test]
fn media_keys_map_to_commands() {
    let (mut world, _dir) = world();
    let (events_tx, events) = mpsc::channel();
    let (info, _info_rx) = mpsc::channel();
    world.insert_resource(MediaKeyChannel {
        events: Mutex::new(events),
        _info: Mutex::new(info),
    });
    for event in [
        MediaKeyEvent::Next,
        MediaKeyEvent::Stop,
        MediaKeyEvent::Pause,
        MediaKeyEvent::Previous,
    ] {
        events_tx.send(event).unwrap();
    }
    world.run_system_once(handle_media_keys).unwrap();
    assert_eq!(
        sent(&mut world),
        [Cmd::NextFile, Cmd::PauseResume, Cmd::PrevFile]
    );
}

#[test]
fn render_toggles_cycle() {
    let (mut world, _dir) = world();
    let scale = |world: &World| world.resource::<RenderSettings>().scale_mode;
    let mut modes = vec![];
    for _ in 0..3 {
        run_cmds(&mut world, &[Cmd::ChangeScale]);
        modes.push(scale(&world));
    }
    assert_eq!(modes, [ScaleMode::Zoom, ScaleMode::Stretch, ScaleMode::Fit]);
    world.resource_mut::<RenderSettings>().scale_mode = ScaleMode::Fixed(2.0);
    run_cmds(&mut world, &[Cmd::ChangeScale]);
    assert_eq!(scale(&world), ScaleMode::Fit);

    let crt = world.resource::<RenderSettings>().crt_effect;
    run_cmds(&mut world, &[Cmd::ToggleCrt, Cmd::ToggleBorder]);
    let render = world.resource::<RenderSettings>();
    assert_eq!(render.crt_effect, !crt);
    assert_eq!(render.border_mode, BorderMode::Stretch);
    run_cmds(&mut world, &[Cmd::ToggleBorder]);
    assert_eq!(world.resource::<RenderSettings>().border_mode, BorderMode::Black);
}

#[test]
fn emu_selection_wraps_and_needs_several_emus() {
    let (mut world, _dir) = world();
    spawn(&mut world, 0);
    run_cmds(&mut world, &[Cmd::NextEmu, Cmd::ToggleAll, Cmd::Maximize]);
    let settings = world.resource::<AppSettings>();
    assert_eq!(
        (settings.current_emu, settings.all_emus, settings.maximized),
        (0, false, false)
    );

    spawn(&mut world, 1);
    spawn(&mut world, 2);
    run_cmds(&mut world, &[Cmd::PrevEmu]);
    assert_eq!(world.resource::<AppSettings>().current_emu, 2);
    run_cmds(&mut world, &[Cmd::NextEmu]);
    assert_eq!(world.resource::<AppSettings>().current_emu, 0);
}

#[test]
fn per_emu_commands_reach_the_current_emu_or_all() {
    let (mut world, _dir) = world();
    let a = spawn(&mut world, 0);
    let b = spawn(&mut world, 1);
    let paused = |world: &World| {
        [a, b].map(|e| world.get::<Emulator>(e).unwrap().paused)
    };
    world.resource_mut::<AppSettings>().current_emu = 1;
    run_cmds(&mut world, &[Cmd::PauseResume]);
    assert_eq!(paused(&world), [false, true]);

    run_cmds(&mut world, &[Cmd::ToggleAll, Cmd::PauseResume]);
    assert_eq!(paused(&world), [true, false]);
}

#[test]
fn reload_steps_back_and_next_all_advances_every_emu() {
    let (mut world, _dir) = world();
    let a = spawn(&mut world, 0);
    let b = spawn(&mut world, 1);
    let run_next = |world: &mut World| {
        [a, b].map(|e| std::mem::take(&mut world.get_mut::<Emulator>(e).unwrap().run_next))
    };
    run_next(&mut world);
    world.resource_mut::<AppSettings>().current_game = 5;
    run_cmds(&mut world, &[Cmd::Reload]);
    assert_eq!(world.resource::<AppSettings>().current_game, 4);
    assert_eq!(run_next(&mut world), [true, false]);

    run_cmds(&mut world, &[Cmd::NextFileAll]);
    assert_eq!(run_next(&mut world), [true, true]);
}
