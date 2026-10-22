use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use bevy::input::mouse::{MouseButtonInput, MouseMotion};
use bevy::input::{ButtonState, InputSystems};
use bevy::prelude::*;
use bevy::render::view::screenshot::Screenshot;
use bevy::render::view::screenshot::save_to_disk;
use bevy::window::{CursorMoved, PrimaryWindow, WindowMode};

use crate::config::{AppSettings, RenderSettings};
use crate::demarc_settings::DemarcSettings;
use crate::egui_settings::ShowSettings;
use crate::emu_file::EmuFile;
use crate::emulator::{Emulator, InputMode};
use crate::frontend::EmuView;
use crate::frontend::FrontendSet;
use crate::fuzzy_list::AllWordsSource;
use crate::media_keys::{self, MediaKeyEvent, MediaKeyInfo};
use crate::navigator::Navigator;
use crate::playlists::{PlaylistPicker, Playlists};
use crate::config::{BorderMode, ScaleMode};
use crate::shader_dialog::ShowShaderDialog;
use crate::ui::{FuzzyListSelect, HudLocation, SetHudText, ShowFuzzyList, UiState};

/// A command triggered by a hotkey while the RightAlt/RightCtrl modifier is
/// held. There is one variant per entry in [`HOTKEYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    NextFile,
    PrevFile,
    SwapDisk,
    ChangeScale,
    ToggleCrt,
    ToggleBorder,
    PauseResume,
    MouseClick,
    ToggleInput,
    ToggleInfo,
    Reset,
    Screenshot,
    ScreenshotAll,
    Warp10,
    Warp30,
    Fullscreen,
    ToggleAll,
    NextEmu,
    PrevEmu,
    Maximize,
    NextFileAll,
    OpenFile,
    Reload,
    Settings,
    ShaderDialog,
    StartOther,
    AddToPlaylist,
    OpenKeyboard,
    ToggleRecording,
}

impl Cmd {
    /// Every variant, so the remote control can name them all.
    pub const ALL: &'static [Cmd] = &[
        Cmd::NextFile,
        Cmd::PrevFile,
        Cmd::SwapDisk,
        Cmd::ChangeScale,
        Cmd::ToggleCrt,
        Cmd::ToggleBorder,
        Cmd::PauseResume,
        Cmd::MouseClick,
        Cmd::ToggleInput,
        Cmd::ToggleInfo,
        Cmd::Reset,
        Cmd::Screenshot,
        Cmd::ScreenshotAll,
        Cmd::Warp10,
        Cmd::Warp30,
        Cmd::Fullscreen,
        Cmd::ToggleAll,
        Cmd::NextEmu,
        Cmd::PrevEmu,
        Cmd::Maximize,
        Cmd::NextFileAll,
        Cmd::OpenFile,
        Cmd::Reload,
        Cmd::Settings,
        Cmd::ShaderDialog,
        Cmd::StartOther,
        Cmd::AddToPlaylist,
        Cmd::OpenKeyboard,
        Cmd::ToggleRecording,
    ];

    /// Look a command up by its `Debug` name, e.g. `"OpenFile"`.
    pub fn from_name(name: &str) -> Option<Cmd> {
        Cmd::ALL.iter().copied().find(|c| format!("{c:?}") == name)
    }

    fn description(self) -> &'static str {
        match self {
            Cmd::NextFile => "Next file",
            Cmd::PrevFile => "Prev file",
            Cmd::SwapDisk => "Swap disk",
            Cmd::ChangeScale => "Change screen scale",
            Cmd::ToggleCrt => "Toggle CRT filter",
            Cmd::ToggleBorder => "Toggle border stretch",
            Cmd::PauseResume => "Pause/Resume",
            Cmd::MouseClick => "Click Left mouse button",
            Cmd::ToggleInput => "Toggle Joystick/Keyboard cursor keys",
            Cmd::ToggleInfo => "Toggle Info",
            Cmd::Reset => "Reset current emulator",
            Cmd::Screenshot => "Screenshot: Current Emulator",
            Cmd::ScreenshotAll => "Screenshot: Whole Screen",
            Cmd::Warp10 => "Warp 10s forward",
            Cmd::Warp30 => "Warp 30s forward",
            Cmd::Fullscreen => "Toggle fullscreen",
            Cmd::ToggleAll => "Toggle all",
            Cmd::NextEmu => "Next emulator",
            Cmd::PrevEmu => "Previous emulator",
            Cmd::Maximize => "(Un)maximize current emulator",
            Cmd::NextFileAll => "Next file in all emulators",
            Cmd::OpenFile => "Open file menu",
            Cmd::Reload => "Reload",
            Cmd::Settings => "Edit settings",
            Cmd::ShaderDialog => "Pick shader preset",
            Cmd::StartOther => "Fade in cross fade emulator",
            Cmd::AddToPlaylist => "Add to playlist",
            Cmd::OpenKeyboard => "Toggle on-screen keyboard",
            Cmd::ToggleRecording => "Start/Stop audio recording",
        }
    }
}

#[derive(Message)]
pub struct CmdMessage(pub Cmd);

/// Binds a key to the [`Cmd`] it triggers.
struct KeyMapping {
    key: KeyCode,
    cmd: Cmd,
    shift: bool,
}

impl KeyMapping {
    const fn new(key: KeyCode, cmd: Cmd) -> Self {
        Self {
            key,
            cmd,
            shift: false,
        }
    }
    const fn shifted(key: KeyCode, cmd: Cmd) -> Self {
        Self {
            key,
            cmd,
            shift: true,
        }
    }

    /// The Nerd-Font keyboard glyph for this key (e.g. the boxed `N`), derived
    /// from the trailing letter of the `KeyCode` (all hotkeys are `Key*`).
    fn glyph(&self) -> char {
        match self.key {
            KeyCode::Tab => '\u{f0312}',
            KeyCode::Enter => '\u{f0311}',
            KeyCode::Space => '\u{f1050}',
            _ => {
                let letter = format!("{:?}", self.key).chars().next_back().unwrap_or('?');
                char::from_u32(letter as u32 - b'A' as u32 + 0xf0b08).unwrap_or('?')
            }
        }
    }

    fn label(&self) -> String {
        if self.shift {
            format!("\u{f0636} + {}", self.glyph())
        } else {
            self.glyph().to_string()
        }
    }
}

const HOTKEYS: &[KeyMapping] = &[
    KeyMapping::new(KeyCode::KeyN, Cmd::NextFile),
    KeyMapping::new(KeyCode::KeyP, Cmd::PrevFile),
    KeyMapping::new(KeyCode::Space, Cmd::NextFile),
    KeyMapping::new(KeyCode::KeyD, Cmd::SwapDisk),
    KeyMapping::new(KeyCode::KeyS, Cmd::ChangeScale),
    KeyMapping::new(KeyCode::KeyC, Cmd::ToggleCrt),
    KeyMapping::new(KeyCode::KeyB, Cmd::ToggleBorder),
    KeyMapping::new(KeyCode::KeyU, Cmd::PauseResume),
    KeyMapping::new(KeyCode::KeyM, Cmd::MouseClick),
    KeyMapping::new(KeyCode::KeyF, Cmd::Fullscreen),
    KeyMapping::new(KeyCode::KeyJ, Cmd::ToggleInput),
    KeyMapping::new(KeyCode::KeyO, Cmd::OpenFile),
    KeyMapping::shifted(KeyCode::KeyO, Cmd::StartOther),
    KeyMapping::new(KeyCode::KeyX, Cmd::Settings),
    KeyMapping::new(KeyCode::KeyZ, Cmd::ShaderDialog),
    KeyMapping::new(KeyCode::KeyI, Cmd::ToggleInfo),
    KeyMapping::new(KeyCode::KeyH, Cmd::AddToPlaylist),
    KeyMapping::new(KeyCode::KeyR, Cmd::Reset),
    KeyMapping::shifted(KeyCode::KeyR, Cmd::ToggleRecording),
    KeyMapping::new(KeyCode::KeyT, Cmd::Screenshot),
    KeyMapping::shifted(KeyCode::KeyT, Cmd::ScreenshotAll),
    KeyMapping::new(KeyCode::KeyW, Cmd::Warp10),
    KeyMapping::shifted(KeyCode::KeyW, Cmd::Warp30),
    KeyMapping::new(KeyCode::Enter, Cmd::Maximize),
    KeyMapping::new(KeyCode::Tab, Cmd::NextEmu),
    KeyMapping::shifted(KeyCode::Tab, Cmd::PrevEmu),
    KeyMapping::new(KeyCode::KeyA, Cmd::ToggleAll),
    KeyMapping::shifted(KeyCode::KeyN, Cmd::NextFileAll),
];

/// Returns the [`Cmd`] bound to whichever hotkey was just pressed this frame,
/// or `None` if no hotkey was pressed.
fn check_hotkey(input: &ButtonInput<KeyCode>) -> Option<Cmd> {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    HOTKEYS
        .iter()
        .find(|m| input.just_pressed(m.key) && m.shift == shift)
        .map(|m| m.cmd)
}

fn handle_hotkey(
    mut settings: ResMut<AppSettings>,
    input: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    ui_state: Res<UiState>,
    mut writer: MessageWriter<CmdMessage>,
) {
    let hot_key_pressed = input.pressed(KeyCode::AltRight) || input.pressed(KeyCode::ControlRight);
    if hot_key_pressed && !ui_state.modal {
        settings.select_box_drawn_at = time.elapsed_secs_f64();
        if let Some(cmd) = check_hotkey(&input) {
            settings.hotkey_pressed_at = 0.0;
            writer.write(CmdMessage(cmd));
        }
    }
}

/// Binds a gamepad button, alone or with a held `modifier`, to a [`Cmd`].
struct PadMapping {
    modifier: Option<GamepadButton>,
    button: GamepadButton,
    cmd: Cmd,
}

impl PadMapping {
    const fn new(button: GamepadButton, cmd: Cmd) -> Self {
        Self {
            modifier: None,
            button,
            cmd,
        }
    }
    const fn with(modifier: GamepadButton, button: GamepadButton, cmd: Cmd) -> Self {
        Self {
            modifier: Some(modifier),
            button,
            cmd,
        }
    }

    fn label(&self) -> String {
        match self.modifier {
            Some(modifier) => format!("{}+{}", pad_name(modifier), pad_name(self.button)),
            None => pad_name(self.button).to_string(),
        }
    }
}

fn pad_name(button: GamepadButton) -> &'static str {
    match button {
        GamepadButton::South => "󰯭",
        GamepadButton::East => "󰯰",
        GamepadButton::West => "󰰲",
        GamepadButton::North => "󰰵",
        GamepadButton::LeftTrigger => "L1",
        GamepadButton::RightTrigger => "R1",
        GamepadButton::LeftTrigger2 => "L2",
        GamepadButton::RightTrigger2 => "R2",
        GamepadButton::Start => "Start",
        GamepadButton::Select => "Select",
        _ => "?",
    }
}

const PAD_MODIFIER: GamepadButton = GamepadButton::LeftTrigger2;

// West/North are X/Y on an Xbox pad, LeftTrigger/LeftTrigger2 are L1/L2.
const PAD_HOTKEYS: &[PadMapping] = &[
    PadMapping::new(GamepadButton::West, Cmd::OpenFile),
    PadMapping::new(GamepadButton::North, Cmd::ToggleInfo),
    PadMapping::new(GamepadButton::RightTrigger, Cmd::NextFile),
    PadMapping::new(GamepadButton::LeftTrigger, Cmd::PrevFile),
    PadMapping::new(GamepadButton::Start, Cmd::PauseResume),
    PadMapping::new(GamepadButton::Select, Cmd::OpenKeyboard),
    PadMapping::with(PAD_MODIFIER, GamepadButton::North, Cmd::ToggleCrt),
    PadMapping::with(PAD_MODIFIER, GamepadButton::West, Cmd::ChangeScale),
    PadMapping::with(PAD_MODIFIER, GamepadButton::RightTrigger, Cmd::Warp10),
    PadMapping::with(PAD_MODIFIER, GamepadButton::LeftTrigger, Cmd::SwapDisk),
    PadMapping::with(PAD_MODIFIER, GamepadButton::Select, Cmd::Settings),
    PadMapping::with(PAD_MODIFIER, GamepadButton::South, Cmd::AddToPlaylist),
];

/// A mapping without a modifier only fires while no modifier button is held.
fn check_pad(pad: &ButtonInput<GamepadButton>) -> Option<Cmd> {
    let held = |b: GamepadButton| pad.pressed(b);
    let any_modifier = PAD_HOTKEYS.iter().filter_map(|m| m.modifier).any(held);
    PAD_HOTKEYS
        .iter()
        .find(|m| pad.just_pressed(m.button) && m.modifier.map_or(!any_modifier, held))
        .map(|m| m.cmd)
}

fn handle_gamepad(
    pads: Query<&Gamepad>,
    ui_state: Res<UiState>,
    mut settings: ResMut<AppSettings>,
    mut writer: MessageWriter<CmdMessage>,
) {
    for pad in &pads {
        if let Some(cmd) = check_pad(pad.digital())
            && (!ui_state.modal || cmd == Cmd::OpenKeyboard)
        {
            settings.hotkey_pressed_at = 0.0;
            writer.write(CmdMessage(cmd));
        }
    }
}

/// The keys a gamepad types while a picker or dialog is open; the last field
/// is whether holding the button repeats.
const PAD_NAV: &[(GamepadButton, KeyCode, bool)] = &[
    (GamepadButton::DPadUp, KeyCode::ArrowUp, true),
    (GamepadButton::DPadDown, KeyCode::ArrowDown, true),
    (GamepadButton::DPadLeft, KeyCode::ArrowLeft, true),
    (GamepadButton::DPadRight, KeyCode::ArrowRight, true),
    (GamepadButton::LeftTrigger, KeyCode::PageUp, true),
    (GamepadButton::RightTrigger, KeyCode::PageDown, true),
    (GamepadButton::South, KeyCode::Enter, false),
    (GamepadButton::East, KeyCode::Escape, false),
];

const PAD_REPEAT_DELAY: f32 = 0.4;
const PAD_REPEAT_RATE: f32 = 0.05;

fn gamepad_navigation(
    pads: Query<&Gamepad>,
    window: Single<Entity, With<PrimaryWindow>>,
    ui_state: Res<UiState>,
    time: Res<Time>,
    mut next_repeat: Local<f32>,
    mut keys: MessageWriter<bevy::input::keyboard::KeyboardInput>,
) {
    let now = time.elapsed_secs();
    let repeat = now >= *next_repeat;
    for pad in &pads {
        for &(button, key, repeats) in PAD_NAV {
            let mut msg = crate::ui::key_message(key, ButtonState::Pressed, *window);
            // Released even after the dialog closed, or the key stays held.
            if pad.just_released(button) {
                msg.state = ButtonState::Released;
                keys.write(msg);
            } else if !ui_state.modal {
            } else if pad.just_pressed(button) {
                *next_repeat = now + PAD_REPEAT_DELAY;
                keys.write(msg);
            } else if repeats && repeat && pad.pressed(button) {
                *next_repeat = now + PAD_REPEAT_RATE;
                msg.repeat = true;
                keys.write(msg);
            }
        }
    }
}

const PAD_MOUSE_DEADZONE: f32 = 0.12;
const PAD_MOUSE_SPEED: f32 = 900.0;

/// Cursor speed in pixels per second for a right stick deflection.
fn stick_velocity(stick: Vec2) -> Vec2 {
    let len = stick.length();
    if len < PAD_MOUSE_DEADZONE {
        return Vec2::ZERO;
    }
    let t = ((len - PAD_MOUSE_DEADZONE) / (1.0 - PAD_MOUSE_DEADZONE)).min(1.0);
    Vec2::new(stick.x, -stick.y) / len * t * t * PAD_MOUSE_SPEED
}

/// The right stick as a mouse, its button as the left mouse button. Steam's
/// default Deck layout sends the right pad as that stick, click included.
fn gamepad_mouse(
    pads: Query<&Gamepad>,
    mut window: Single<(Entity, &mut Window), With<PrimaryWindow>>,
    time: Res<Time>,
    mut buttons: MessageWriter<MouseButtonInput>,
    mut motion: MessageWriter<MouseMotion>,
    mut moved: MessageWriter<CursorMoved>,
) {
    let (entity, window) = &mut *window;
    for pad in &pads {
        let delta = stick_velocity(pad.right_stick()) * time.delta_secs();
        if delta != Vec2::ZERO {
            let size = window.size();
            let position = (window.cursor_position().unwrap_or(size / 2.0) + delta)
                .clamp(Vec2::ZERO, size - 1.0);
            window.set_cursor_position(Some(position));
            motion.write(MouseMotion { delta });
            moved.write(CursorMoved {
                window: *entity,
                position,
                delta: Some(delta),
            });
        }
        let state = if pad.just_pressed(GamepadButton::RightThumb) {
            ButtonState::Pressed
        } else if pad.just_released(GamepadButton::RightThumb) {
            ButtonState::Released
        } else {
            continue;
        };
        buttons.write(MouseButtonInput {
            button: MouseButton::Left,
            state,
            window: *entity,
        });
    }
}

/// The rows of the command overlay, and the [`Cmd`] each one runs.
fn command_list(pad: bool) -> Vec<(String, Cmd)> {
    if pad {
        PAD_HOTKEYS
            .iter()
            .map(|m| (format!(" {:<6} {} ", m.label(), m.cmd.description()), m.cmd))
            .collect()
    } else {
        HOTKEYS
            .iter()
            .map(|m| (format!(" {} {} ", m.label(), m.cmd.description()), m.cmd))
            .collect()
    }
}

fn handle_textlist(
    mut settings: ResMut<AppSettings>,
    input: Res<ButtonInput<KeyCode>>,
    pads: Query<&Gamepad>,
    mut pad_list: Local<bool>,
    mut file_reader: MessageReader<FuzzyListSelect>,
    mut writer: MessageWriter<CmdMessage>,
    mut show_list: MessageWriter<ShowFuzzyList>,
    time: Res<Time>,
    ui_state: Res<UiState>,
    // The entry whose downloads the list opened by Shift+Enter is showing, kept
    // until that list reports back (its own `item` is a URL index, not a file).
) {
    for msg in file_reader.read() {
        if msg.id == 99
            && let Some(&(_, cmd)) = command_list(*pad_list).get(msg.item)
        {
            writer.write(CmdMessage(cmd));
        }
    }
    let pad_pressed = pads.iter().any(|p| p.just_pressed(PAD_MODIFIER));
    let pad_released = pads.iter().any(|p| p.just_released(PAD_MODIFIER));
    let hot_key_pressed = pad_pressed
        || input.just_pressed(KeyCode::AltRight)
        || input.just_pressed(KeyCode::ControlRight);
    let hot_key_released = pad_released
        || input.just_released(KeyCode::AltRight)
        || input.just_released(KeyCode::ControlRight);

    if hot_key_pressed {
        settings.hotkey_pressed_at = time.elapsed_secs();
    } else if hot_key_released {
        // TODO: We sometimes get quick PRESS/RELEASE/PRESS for only press
        let modal = ui_state.modal;
        if modal {
            return;
        }
        if time.elapsed_secs() - settings.hotkey_pressed_at < 0.35 {
            *pad_list = pad_released || crate::on_steam_deck();
            let lines = command_list(*pad_list)
                .into_iter()
                .map(|(line, _)| line)
                .collect::<Vec<_>>();
            let source = AllWordsSource::new(lines);
            show_list.write(ShowFuzzyList {
                id: 99,
                source: Arc::new(source),
                prompt: None,
                selected: None,
                title: String::new(),
                title_select: -1,
            });
        }
    }
}

/// Steam's on-screen keyboard; Deck only, since `steam <url>` starts a stopped client.
fn steam_keyboard(open: bool) {
    if !crate::on_steam_deck() {
        return;
    }
    let action = if open { "open" } else { "close" };
    let url = format!("steam://{action}/keyboard");
    std::thread::spawn(move || {
        let _ = std::process::Command::new("steam").arg(url).status();
    });
}

fn close_keyboard_on_load() {
    steam_keyboard(false);
}

fn reload(emu: &mut Emulator, settings: &mut AppSettings, navigator: &mut Navigator) {
    settings.current_game -= 1;
    if let Some(launch) = navigator.current_launch.as_mut() {
        launch.index -= 1;
    }
    emu.run_next = true;
}

pub(crate) fn handle_cmd(
    mut cmds: MessageReader<CmdMessage>,
    mut emus: Query<(&mut Emulator, &EmuView)>,
    mut settings: ResMut<AppSettings>,
    mut render: ResMut<RenderSettings>,
    mut navigator: ResMut<Navigator>,
    // Optional: `--headless` has no window, and a bare `Single` would skip the
    // whole system, dropping every command a remote-control script sends.
    mut window: Option<Single<&mut Window, With<PrimaryWindow>>>,
    time: Res<Time>,
    mut writer: MessageWriter<SetHudText>,
    mut show_list: MessageWriter<ShowFuzzyList>,
    mut show_settings: MessageWriter<ShowSettings<DemarcSettings>>,
    mut show_shader: MessageWriter<ShowShaderDialog>,
    mut demo_settings: ResMut<DemarcSettings>,
    mut commands: Commands,
    dj: Option<Res<crate::dj::DjWindow>>,
    playlists: Res<Playlists>,
) {
    let dj_focused = crate::dj::has_focus(dj.as_deref());
    let mut show_info = false;
    let count = emus.iter().filter(|(emu, _)| !emu.is_crossfade).count();
    let multi = count > 1;
    for cmd in cmds.read() {
        debug!("Received command: {:?}", cmd.0);
        match cmd.0 {
            Cmd::ToggleCrt => {
                render.crt_effect = !render.crt_effect;
                writer.write(SetHudText {
                    text: (if render.crt_effect {
                        "Filter on"
                    } else {
                        "Filter off"
                    })
                    .into(),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(1),
                    location: HudLocation::TopLeft,
                });
            }
            Cmd::ToggleBorder => {
                render.border_mode = if render.border_mode == BorderMode::Stretch {
                    BorderMode::Black
                } else {
                    BorderMode::Stretch
                };
            }
            Cmd::ChangeScale => {
                render.scale_mode = match render.scale_mode {
                    ScaleMode::Stretch => ScaleMode::Fit,
                    ScaleMode::Fit => ScaleMode::Zoom,
                    ScaleMode::Zoom => ScaleMode::Stretch,
                    // The fixed integer scales are CLI-only; the keyboard cycle
                    // returns to the aspect-preserving modes.
                    ScaleMode::Fixed(_) => ScaleMode::Fit,
                };
                writer.write(SetHudText {
                    text: format!("{:?}", render.scale_mode),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(1),
                    location: HudLocation::TopLeft,
                });
            }
            Cmd::Fullscreen => {
                if let Some(window) = window.as_mut() {
                    window.mode = match window.mode {
                        WindowMode::Windowed => {
                            WindowMode::BorderlessFullscreen(MonitorSelection::Current)
                        }
                        _ => WindowMode::Windowed,
                    };
                    // So the settings dialog opens showing where the window
                    // actually is, and doesn't undo this the next time it is
                    // applied. This is the only hotkey that moves a field the
                    // dialog also owns.
                    demo_settings.fullscreen = window.mode != WindowMode::Windowed;
                }
            }
            Cmd::ToggleAll if multi => {
                settings.all_emus = !settings.all_emus;
            }
            Cmd::NextEmu if multi => {
                if settings.show_info {
                    show_info = true;
                }
                settings.current_emu = (settings.current_emu + 1) % count;
            }
            Cmd::PrevEmu if multi => {
                if settings.show_info {
                    show_info = true;
                }
                settings.current_emu = (settings.current_emu + count - 1) % count;
            }
            Cmd::Maximize if multi => {
                settings.maximized = !settings.maximized;
                if settings.show_info && settings.maximized {
                    show_info = true;
                }
                if !settings.maximized {
                    writer.write(SetHudText {
                        location: HudLocation::InfoText,
                        ..Default::default()
                    });
                }
            }
            Cmd::OpenFile => {
                navigator.open(&mut show_list);
            }
            Cmd::Settings => {
                show_settings.write(ShowSettings::new(demo_settings.clone(), "Settings"));
            }
            Cmd::ShaderDialog => {
                show_shader.write(ShowShaderDialog);
            }
            Cmd::OpenKeyboard => {
                steam_keyboard(true);
            }
            _ => {}
        }
        // While the DJ window has the keyboard the per-emulator commands belong
        // to the cue it shows. Advancing is the exception: the cross fade
        // pipeline takes a load off the view that asked for it and runs it in
        // the cue itself, so those stay where they are.
        let advance = matches!(
            cmd.0,
            Cmd::NextFile | Cmd::PrevFile | Cmd::Reload | Cmd::NextFileAll
        );
        let cue = dj_focused && !advance;
        for (mut emu, view) in &mut emus {
            if emu.is_crossfade != cue {
                continue;
            }
            let i = view.index;
            if show_info && i == settings.current_emu {
                writer.write_batch(emu.info_hud(Duration::ZERO, Duration::from_secs(2)));
            }
            if cmd.0 == Cmd::NextFileAll {
                emu.run_next = true;
            }
            if cue || settings.all_emus || i == settings.current_emu {
                match cmd.0 {
                    Cmd::MouseClick => emu.set_mouse_buttons(0x1),
                    Cmd::ToggleInput => {
                        emu.input_mode = emu.input_mode.next();
                        let text = match emu.input_mode {
                            InputMode::Keyboard => "\u{f030c}",
                            InputMode::Joystick1 => "\u{f0297} \u{b9}",
                            InputMode::Joystick2 => "\u{f0297} \u{b2}",
                        };
                        writer.write(SetHudText {
                            text: text.into(),
                            delay: Duration::from_secs(0),
                            duration: Duration::from_secs(1),
                            location: HudLocation::BottomLeft,
                        });
                    }
                    Cmd::Reload => reload(&mut emu, &mut settings, &mut navigator),
                    Cmd::PauseResume => {
                        emu.paused = !emu.paused;
                        if !emu.is_image {
                            if emu.paused {
                                writer.write(SetHudText {
                                    location: HudLocation::TopRight,
                                    duration: Duration::from_secs(1500),
                                    text: "\u{f03e4}".into(),
                                    ..Default::default()
                                });
                            } else {
                                writer.write(SetHudText {
                                    location: HudLocation::TopRight,
                                    ..Default::default()
                                });
                            }
                        }
                    }
                    Cmd::SwapDisk => {
                        let nd = emu.get_number_of_disks();
                        if nd > 0 {
                            emu.disk_no = (emu.disk_no + 1) % nd;
                        }
                        let disk_no = emu.disk_no;
                        emu.set_disk(disk_no);
                        let floppy = emu.work_file.get_meta_or("system", "").starts_with("C64");
                        let d = emu.disk_no + 1;

                        writer.write(SetHudText {
                            location: HudLocation::BottomLeft,
                            duration: Duration::from_millis(1500),
                            text: if floppy {
                                format!("\u{f09ef} #{d}")
                            } else {
                                format!("\u{f0249} #{d}")
                            },
                            ..Default::default()
                        });
                    }
                    Cmd::Reset if emu.work_file.get_meta_or("system", "") == "Windows" => {
                        reload(&mut emu, &mut settings, &mut navigator);
                    }
                    Cmd::Reset if emu.core.is_some() => {
                        emu.reset();
                    }
                    Cmd::ToggleRecording => {
                        if emu.sink.recording_time().is_some() {
                            emu.sink.stop_recording();
                        } else if emu.core.is_none() {
                            emu.record_pending = match emu.record_pending {
                                Some(_) => None,
                                None => Some(demo_settings.recording.options()),
                            };
                        } else {
                            let title = emu.work_file.get_meta_or("title", "audio");
                            let name = format!("{}-{}.wav", title, time.elapsed_secs() as i32);
                            emu.sink.start_recording(name, demo_settings.recording.options());
                        }
                    }
                    Cmd::AddToPlaylist if emu.core.is_some() => {
                        let picker = Arc::new(PlaylistPicker::new(&playlists, &emu.emu_file));
                        show_list.write(ShowFuzzyList {
                            id: PLAYLIST_PICKER,
                            source: picker.clone(),
                            prompt: Some(String::new()),
                            selected: None,
                            title: "Add to playlist".into(),
                            title_select: -1,
                        });
                        commands.insert_resource(PlaylistPick {
                            file: emu.emu_file.clone(),
                            fetched: emu.work_file.get_meta_or("fetched", ""),
                            picker,
                        });
                    }
                    Cmd::ToggleInfo => {
                        if emu.show_info {
                            writer.write_batch(
                                [HudLocation::InfoText, HudLocation::Favorite].map(|location| {
                                    SetHudText {
                                        location,
                                        ..Default::default()
                                    }
                                }),
                            );
                        } else {
                            writer.write_batch(
                                emu.info_hud(Duration::ZERO, Duration::from_secs(5000)),
                            );
                        }
                        emu.show_info = !emu.show_info;
                    }
                    Cmd::NextFile => {
                        emu.run_next = true;
                        debug!(
                            "{} vs {}",
                            settings.current_game,
                            settings.default_db().len()
                        );
                    }
                    Cmd::PrevFile => {
                        emu.run_prev = true;
                        debug!(
                            "{} vs {}",
                            settings.current_game,
                            settings.default_db().len()
                        );
                    }
                    Cmd::Warp10 => {
                        let text = "\u{f0d71}".to_string();
                        emu.skip(10 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            // For safety, should be hidden automatically
                            duration: Duration::from_secs(10),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::Warp30 => {
                        let text = "\u{f0d06}".to_string();
                        emu.skip(30 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            // For safety, should be hidden automatically
                            duration: Duration::from_secs(30),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::ScreenshotAll => {
                        let name = format!("screenshot-{}.png", time.elapsed_secs() as i32);
                        commands
                            .spawn(Screenshot::primary_window())
                            .observe(save_to_disk(name));
                    }
                    Cmd::Screenshot => {
                        let title = emu.work_file.get_meta_or("title", "shot");
                        let name = format!("{}-{}.png", title, time.elapsed_secs() as i32);
                        _ = emu.save_png(&name);
                        writer.write(SetHudText {
                            text: format!("Screenshot: {name}"),
                            delay: Duration::from_secs(0),
                            duration: Duration::from_secs(1),
                            location: HudLocation::TopLeft,
                        });
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Holds the channels to the background MPRIS listener. The `mpsc` ends are
/// wrapped in a `Mutex` so the resource is `Sync`; we keep the info sender alive
/// (even though we don't push state to it) so the listener's channel stays open.
#[derive(Resource)]
struct MediaKeyChannel {
    events: Mutex<mpsc::Receiver<MediaKeyEvent>>,
    _info: Mutex<mpsc::Sender<MediaKeyInfo>>,
}

/// Start the media-key listener and store its channels.
fn init_media_keys(mut commands: Commands) {
    let (info, events) = media_keys::start();
    commands.insert_resource(MediaKeyChannel {
        events: Mutex::new(events),
        _info: Mutex::new(info),
    });
}

/// Translate media-key presses into [`Cmd`]s, mirroring the Ctrl-N / Ctrl-P
/// hotkeys: Next plays the next file, Play/Pause toggles pause.
fn handle_media_keys(channel: Res<MediaKeyChannel>, mut writer: MessageWriter<CmdMessage>) {
    let Ok(events) = channel.events.lock() else {
        return;
    };
    while let Ok(event) = events.try_recv() {
        let cmd = match event {
            MediaKeyEvent::Next => Some(Cmd::NextFile),
            MediaKeyEvent::PlayPause | MediaKeyEvent::Play | MediaKeyEvent::Pause => {
                Some(Cmd::PauseResume)
            }
            MediaKeyEvent::Previous => Some(Cmd::PrevFile),
            MediaKeyEvent::Stop => None,
        };
        if let Some(cmd) = cmd {
            writer.write(CmdMessage(cmd));
        }
    }
}

const PLAYLIST_PICKER: usize = 98;

/// The release the open playlist picker is for.
#[derive(Resource)]
struct PlaylistPick {
    file: EmuFile,
    fetched: String,
    picker: Arc<PlaylistPicker>,
}

fn handle_playlist_pick(
    mut reader: MessageReader<FuzzyListSelect>,
    pick: Option<Res<PlaylistPick>>,
    mut playlists: ResMut<Playlists>,
    mut navigator: ResMut<Navigator>,
    mut emus: Query<&mut Emulator>,
    mut writer: MessageWriter<SetHudText>,
) {
    let Some(pick) = pick else {
        return;
    };
    for msg in reader.read() {
        if msg.id != PLAYLIST_PICKER {
            continue;
        }
        let index = if msg.item < playlists.lists.len() {
            msg.item
        } else {
            match playlists.create(&pick.picker.query()) {
                Some(index) => index,
                None => {
                    writer.write(SetHudText {
                        text: "Bad playlist name".into(),
                        duration: Duration::from_secs(1),
                        location: HudLocation::TopLeft,
                        ..Default::default()
                    });
                    continue;
                }
            }
        };
        let added = playlists.toggle(index, &pick.file, &pick.fetched);
        let list = &playlists.lists[index];
        navigator.add_playlist(&list.name, list.files);
        if index == 0 {
            for mut emu in &mut emus {
                let favorite = playlists.is_favorite(&emu.emu_file);
                if emu.favorite != favorite {
                    emu.favorite = favorite;
                    if emu.show_info {
                        writer.write_batch(
                            emu.info_hud(Duration::ZERO, Duration::from_secs(5000)),
                        );
                    }
                }
            }
        }
        writer.write(SetHudText {
            text: if added {
                format!("Added to {}", list.name)
            } else {
                format!("Removed from {}", list.name)
            },
            duration: Duration::from_secs(1),
            location: HudLocation::TopLeft,
            ..Default::default()
        });
    }
}

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CmdMessage>()
            .insert_resource(Playlists::load(&crate::playlists::default_dir()))
            .add_systems(Startup, init_media_keys)
            .add_systems(
                Update,
                (
                    handle_hotkey.in_set(FrontendSet::Input),
                    handle_media_keys.in_set(FrontendSet::Input),
                    handle_gamepad.in_set(FrontendSet::Input),
                    handle_textlist,
                    handle_playlist_pick,
                    handle_cmd.run_if(on_message::<CmdMessage>),
                    close_keyboard_on_load.run_if(on_message::<crate::loading::LoadFile>),
                ),
            )
            .add_systems(
                PreUpdate,
                (gamepad_navigation, gamepad_mouse).before(InputSystems),
            );
    }
}

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
