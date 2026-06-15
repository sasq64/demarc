use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use bevy::window::{PrimaryWindow, WindowMode};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, save_to_disk},
};

use crate::AppSettings;
use crate::emulator::{Emulator, InputMode};
use crate::hud::{HudLocation, SetHudText, TextList, TextListSelect};
use crate::media_keys::{self, MediaKeyEvent, MediaKeyInfo};
use crate::post_process::{BorderMode, ScaleMode};
use crate::utils::{GameInfo, SystemType, WorkingFile};

/// A command triggered by a hotkey while the RightAlt/RightCtrl modifier is
/// held. There is one variant per entry in [`HOTKEYS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    NextFile,
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
    Warp10,
    Warp30,
    Fullscreen,
    ToggleAll,
    NextEmu,
    PrevEmu,
    Maximize,
    NextFileAll,
}

#[derive(Message)]
pub struct CmdMessage(pub Cmd, pub bool);

/// Binds a key to the [`Cmd`] it triggers, plus a description shown in the
/// RightAlt overlay (see [`handle_textlist`]).
struct KeyMapping {
    key: KeyCode,
    description: &'static str,
    cmd: Cmd,
    shift: bool,
}

impl KeyMapping {
    const fn new(key: KeyCode, description: &'static str, cmd: Cmd) -> Self {
        Self {
            key,
            description,
            cmd,
            shift: false,
        }
    }
    const fn shifted(key: KeyCode, description: &'static str, cmd: Cmd) -> Self {
        Self {
            key,
            description,
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
}

const HOTKEYS: &[KeyMapping] = &[
    KeyMapping::new(KeyCode::KeyN, "Next file", Cmd::NextFile),
    KeyMapping::new(KeyCode::Space, "Next file", Cmd::NextFile),
    KeyMapping::new(KeyCode::KeyD, "Swap disk", Cmd::SwapDisk),
    KeyMapping::new(KeyCode::KeyS, "Change screen scale", Cmd::ChangeScale),
    KeyMapping::new(KeyCode::KeyC, "Toggle CRT filter", Cmd::ToggleCrt),
    KeyMapping::new(KeyCode::KeyB, "Toggle border stretch", Cmd::ToggleBorder),
    KeyMapping::new(KeyCode::KeyP, "Pause/Resume", Cmd::PauseResume),
    KeyMapping::new(KeyCode::KeyM, "Click Left mouse button", Cmd::MouseClick),
    KeyMapping::new(KeyCode::KeyF, "Toggle fullscreen", Cmd::Fullscreen),
    KeyMapping::new(
        KeyCode::KeyJ,
        "Toggle Joystick/Keyboard cursor keys",
        Cmd::ToggleInput,
    ),
    KeyMapping::new(KeyCode::KeyI, "Toggle Info", Cmd::ToggleInfo),
    KeyMapping::new(KeyCode::KeyR, "Reset current emulator", Cmd::Reset),
    KeyMapping::new(KeyCode::KeyT, "Take screenshot", Cmd::Screenshot),
    KeyMapping::new(KeyCode::KeyW, "Warp 10s forward", Cmd::Warp10),
    KeyMapping::shifted(KeyCode::KeyW, "Warp 30s forward", Cmd::Warp30),
    KeyMapping::new(
        KeyCode::Enter,
        "(Un)maximize current emulator",
        Cmd::Maximize,
    ),
    KeyMapping::new(KeyCode::Tab, "Next emulator", Cmd::NextEmu),
    KeyMapping::shifted(KeyCode::Tab, "Previous emulator", Cmd::PrevEmu),
    KeyMapping::new(KeyCode::KeyA, "Toggle all", Cmd::ToggleAll),
    KeyMapping::shifted(
        KeyCode::KeyN,
        "Next file in all emulators",
        Cmd::NextFileAll,
    ),
];

/// Returns the [`Cmd`] bound to whichever hotkey was just pressed this frame,
/// or `None` if no hotkey was pressed.
pub fn check_hotkey(input: &ButtonInput<KeyCode>) -> Option<Cmd> {
    let shift = input.pressed(KeyCode::ShiftLeft) || input.pressed(KeyCode::ShiftRight);
    HOTKEYS
        .iter()
        .find(|m| input.just_pressed(m.key) && m.shift == shift)
        .map(|m| m.cmd)
}

/// Capture the actual rendered window content and write it to `screenshot.png`.
fn screenshot(commands: &mut Commands, name: impl Into<String>) {
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(name.into()));
}

fn handle_textlist(
    mut commands: Commands,
    mut settings: ResMut<AppSettings>,
    asset_server: Res<AssetServer>,
    input: Res<ButtonInput<KeyCode>>,
    mut reader: MessageReader<TextListSelect>,
    mut writer: MessageWriter<CmdMessage>,
    time: Res<Time>,
) {
    for index in reader.read() {
        if index.0 < HOTKEYS.len() {
            let cmd = HOTKEYS[index.0].cmd;
            writer.write(CmdMessage(cmd, false));
            if let Some(e) = settings.text_list.take() {
                commands.entity(e).despawn();
            }
        }
    }
    let hot_key_pressed =
        input.just_pressed(KeyCode::AltRight) || input.just_pressed(KeyCode::ControlRight);
    let hot_key_released =
        input.just_released(KeyCode::AltRight) || input.just_released(KeyCode::ControlRight);

    if hot_key_pressed {
        settings.hotkey_pressed = time.elapsed_secs();
    } else if hot_key_released {
        if time.elapsed_secs() - settings.hotkey_pressed < 0.35 {
            if let Some(e) = settings.text_list.take() {
                commands.entity(e).despawn();
            } else {
                let font: Handle<Font> = asset_server.load("font.ttf");
                let lines = HOTKEYS
                    .iter()
                    .map(|m| {
                        if m.shift {
                            format!(" \u{f0636}+{} {} ", m.glyph(), m.description)
                        } else {
                            format!(" {} {} ", m.glyph(), m.description)
                        }
                    })
                    .collect::<Vec<_>>();
                let entity = TextList::spawn(&mut commands, font, lines, 8, 500.0);
                settings.text_list = Some(entity);
            }
        }
    } else if input.just_pressed(KeyCode::Escape)
        && let Some(e) = settings.text_list.take()
    {
        commands.entity(e).despawn();
    }
}

fn get_system_name(work_file: &WorkingFile) -> String {
    let tags = &work_file.settings;
    let ste = tags.get("hatari_machinetype").is_some_and(|v| v == "ste");
    let a1200 = tags.get("puae_model").is_some_and(|v| v == "A1200");
    let mut base = match work_file.system_type {
        SystemType::C64 => "C64",
        SystemType::Amiga => "Amiga",
        SystemType::Amstrad => "Amstrad CPC",
        SystemType::Megadrive => "Megadrive",
        SystemType::ZXSpectrum => "ZX Spectrum",
        SystemType::AtariST => {
            if ste {
                "Atari STE"
            } else {
                "Atari ST"
            }
        }
        SystemType::Atari2600 => "Atari 2600",
        SystemType::SuperNintendo => "SNES",
        SystemType::AtariXL => "Atari XL",
        SystemType::Unknown => "Unknown",
    }
    .to_string();
    if work_file.system_type == SystemType::Amiga {
        if a1200 {
            base += " (AGA)";
        } else {
            base += " 500";
        }
    }
    base
}

pub fn get_info_text(work_file: &WorkingFile) -> String {
    let system = get_system_name(work_file);
    let GameInfo { title, group, year } = &work_file.game_info;
    let year = if year.is_empty() {
        "".into()
    } else {
        format!(" ({year})")
    };

    format!("\"{title}\"\n{group}\n{system}{year}")
}

fn handle_cmd(
    mut cmds: MessageReader<CmdMessage>,
    mut commands: Commands,
    mut emus: Query<&mut Emulator>,
    mut settings: ResMut<AppSettings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    time: Res<Time>,
    mut writer: MessageWriter<SetHudText>,
) {
    let mut show_info = false;
    let count = emus.iter().count();
    let multi = count > 1;
    for cmd in cmds.read() {
        debug!("CMD: {:?}", cmd.0);
        match cmd.0 {
            Cmd::ToggleCrt => {
                settings.crt_effect = !settings.crt_effect;
                writer.write(SetHudText {
                    text: (if settings.crt_effect {
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
                settings.border_mode = if settings.border_mode == BorderMode::Stretch {
                    BorderMode::Black
                } else {
                    BorderMode::Stretch
                };
            }
            Cmd::ChangeScale => {
                settings.scale_mode = match settings.scale_mode {
                    ScaleMode::Stretch => ScaleMode::Fit,
                    ScaleMode::Fit => ScaleMode::Zoom,
                    ScaleMode::Zoom => ScaleMode::Stretch,
                };
                writer.write(SetHudText {
                    text: format!("{:?}", settings.scale_mode),
                    delay: Duration::from_secs(0),
                    duration: Duration::from_secs(1),
                    location: HudLocation::TopLeft,
                });
            }
            Cmd::Fullscreen => {
                window.mode = match window.mode {
                    WindowMode::Windowed => {
                        WindowMode::BorderlessFullscreen(MonitorSelection::Current)
                    }
                    _ => WindowMode::Windowed,
                };
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
            _ => {}
        }
        for (i, mut emu) in &mut emus.iter_mut().enumerate() {
            if show_info && i == settings.current_emu {
                writer.write(SetHudText {
                    text: get_info_text(&emu.work_file),
                    duration: Duration::from_secs(2),
                    location: HudLocation::InfoText,
                    ..Default::default()
                });
            }
            if cmd.0 == Cmd::NextFileAll {
                emu.run_next = true;
            }
            if settings.all_emus || i == settings.current_emu {
                match cmd.0 {
                    Cmd::MouseClick => emu.set_mouse_buttons(true, false, false),
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
                    Cmd::PauseResume => {
                        emu.paused = !emu.paused;
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
                    Cmd::SwapDisk => {
                        let nd = emu.get_number_of_disks();
                        if nd > 0 {
                            emu.disk_no = (emu.disk_no + 1) % nd;
                        }
                        let disk_no = emu.disk_no;
                        emu.set_disk(disk_no);
                        let floppy = emu.work_file.system_type == SystemType::C64;
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
                    Cmd::Reset => {
                        emu.reset();
                    }
                    Cmd::ToggleInfo => {
                        if emu.show_info {
                            writer.write(SetHudText {
                                location: HudLocation::InfoText,
                                ..Default::default()
                            });
                        } else {
                            writer.write(SetHudText {
                                text: get_info_text(&emu.work_file),
                                delay: Duration::from_secs(0),
                                duration: Duration::from_secs(5000),
                                location: HudLocation::InfoText,
                            });
                        }
                        emu.show_info = !emu.show_info;
                    }
                    Cmd::NextFile => {
                        emu.run_next = true;
                        debug!("{} vs {}", settings.current_game, settings.games.len());
                    }
                    Cmd::Warp10 => {
                        let text = "\u{f0d71}".to_string();
                        emu.skip(10 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            duration: Duration::from_secs(1500),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::Warp30 => {
                        let text = "\u{f0d06}".to_string();
                        emu.skip(30 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            duration: Duration::from_secs(1500),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::Screenshot => {
                        let name = format!(
                            "{}-{}.png",
                            emu.work_file.game_info.title,
                            time.elapsed_secs() as i32
                        );
                        screenshot(&mut commands, &name);
                        writer.write(SetHudText {
                            text: format!("Screenshot: {name}"),
                            delay: Duration::from_secs(0),
                            duration: Duration::from_secs(5000),
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
            MediaKeyEvent::Previous | MediaKeyEvent::Stop => None,
        };
        if let Some(cmd) = cmd {
            writer.write(CmdMessage(cmd, false));
        }
    }
}

pub struct CommandPlugin;

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CmdMessage>();
        app.add_systems(Startup, init_media_keys);
        app.add_systems(
            Update,
            (
                handle_textlist,
                handle_media_keys,
                handle_cmd.run_if(on_message::<CmdMessage>),
            ),
        );
    }
}
