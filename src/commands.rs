use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::render::view::screenshot::Screenshot;
use bevy::render::view::screenshot::save_to_disk;
use bevy::window::{PrimaryWindow, WindowMode};

use crate::config::{AppSettings, RenderSettings};
use crate::demarc_settings::DemarcSettings;
use crate::egui_settings::ShowSettings;
use crate::emu_file::UrlList;
use crate::emu_file::{EmuFile, FileSource};
use crate::emulator::{Emulator, InputMode};
use crate::frontend::EmuView;
use crate::frontend::FrontendSet;
use crate::fuzzy_list::AllWordsSource;
use crate::fuzzy_list::ListIcon;
use crate::fuzzy_list::{FuzzySource, IndexedSource};
use crate::media_keys::{self, MediaKeyEvent, MediaKeyInfo};
use crate::navigator::Navigator;
use crate::playlists::{PlaylistPicker, Playlists};
use crate::post_process::{BorderMode, ScaleMode};
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
    ];

    /// Look a command up by its `Debug` name, e.g. `"OpenFile"`.
    pub fn from_name(name: &str) -> Option<Cmd> {
        Cmd::ALL.iter().copied().find(|c| format!("{c:?}") == name)
    }
}

#[derive(Message)]
pub struct CmdMessage(pub Cmd);

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
    KeyMapping::new(KeyCode::KeyP, "Prev file", Cmd::PrevFile),
    KeyMapping::new(KeyCode::Space, "Next file", Cmd::NextFile),
    KeyMapping::new(KeyCode::KeyD, "Swap disk", Cmd::SwapDisk),
    KeyMapping::new(KeyCode::KeyS, "Change screen scale", Cmd::ChangeScale),
    KeyMapping::new(KeyCode::KeyC, "Toggle CRT filter", Cmd::ToggleCrt),
    KeyMapping::new(KeyCode::KeyB, "Toggle border stretch", Cmd::ToggleBorder),
    KeyMapping::new(KeyCode::KeyU, "Pause/Resume", Cmd::PauseResume),
    KeyMapping::new(KeyCode::KeyM, "Click Left mouse button", Cmd::MouseClick),
    KeyMapping::new(KeyCode::KeyF, "Toggle fullscreen", Cmd::Fullscreen),
    KeyMapping::new(
        KeyCode::KeyJ,
        "Toggle Joystick/Keyboard cursor keys",
        Cmd::ToggleInput,
    ),
    KeyMapping::new(KeyCode::KeyO, "Open file menu", Cmd::OpenFile),
    KeyMapping::shifted(
        KeyCode::KeyO,
        "Fade in cross fade emulator",
        Cmd::StartOther,
    ),
    KeyMapping::new(KeyCode::KeyX, "Edit settings", Cmd::Settings),
    KeyMapping::new(KeyCode::KeyZ, "Pick shader preset", Cmd::ShaderDialog),
    KeyMapping::new(KeyCode::KeyI, "Toggle Info", Cmd::ToggleInfo),
    KeyMapping::new(KeyCode::KeyH, "Add to playlist", Cmd::AddToPlaylist),
    KeyMapping::new(KeyCode::KeyR, "Reset current emulator", Cmd::Reset),
    KeyMapping::new(
        KeyCode::KeyT,
        "Screenshot: Current Emulator",
        Cmd::Screenshot,
    ),
    KeyMapping::shifted(
        KeyCode::KeyT,
        "Screenshot: Whole Screen",
        Cmd::ScreenshotAll,
    ),
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

fn handle_textlist(
    mut settings: ResMut<AppSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut file_reader: MessageReader<FuzzyListSelect>,
    mut writer: MessageWriter<CmdMessage>,
    mut show_list: MessageWriter<ShowFuzzyList>,
    time: Res<Time>,
    ui_state: Res<UiState>,
    // The entry whose downloads the list opened by Shift+Enter is showing, kept
    // until that list reports back (its own `item` is a URL index, not a file).
) {
    for msg in file_reader.read() {
        if msg.id == 99 && msg.item < HOTKEYS.len() {
            let cmd = HOTKEYS[msg.item].cmd;
            writer.write(CmdMessage(cmd));
        }
    }
    let hot_key_pressed =
        input.just_pressed(KeyCode::AltRight) || input.just_pressed(KeyCode::ControlRight);
    let hot_key_released =
        input.just_released(KeyCode::AltRight) || input.just_released(KeyCode::ControlRight);

    if hot_key_pressed {
        settings.hotkey_pressed_at = time.elapsed_secs();
    } else if hot_key_released {
        // TODO: We sometimes get quick PRESS/RELEASE/PRESS for only press
        let modal = ui_state.modal;
        if modal {
            return;
        }
        if time.elapsed_secs() - settings.hotkey_pressed_at < 0.35 {
            let lines = HOTKEYS
                .iter()
                .map(|m| {
                    if m.shift {
                        format!(" \u{f0636} + {} {} ", m.glyph(), m.description)
                    } else {
                        format!(" {} {} ", m.glyph(), m.description)
                    }
                })
                .collect::<Vec<_>>();
            let source = AllWordsSource::new(lines);
            show_list.write(ShowFuzzyList {
                id: 99,
                source: Arc::new(source),
                prompt: None,
                selected: None,
                title: String::new(),
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconMode {
    Platforms,
    Categories,
    PlatformOnly,
    CategoryOnly,
    All,
}

/// Backs the file picker: an [`IndexedSource`] over the one-line names shown in
/// the list, paired with the entries themselves — the fuller detail (year,
/// type, party, …) shown in the info field below the list, and the entry a
/// selection is *of*, handed back by [`FuzzySource::get_data`].
#[derive(Clone)]
pub struct PickerSource {
    mode: IconMode,
    names: IndexedSource,
    /// Index into `emu_files` per row, in the order `names` holds them: the ids
    /// a search reports are rows of this subset, not of the whole list. `None`
    /// means every entry, in order, so a row id *is* its index.
    subset: Option<Vec<u32>>,
    emu_files: &'static [EmuFile],
    width: u32,
}

impl PickerSource {
    pub(crate) fn new(
        files: &'static [EmuFile],
        subset: Option<Vec<u32>>,
        icon_mode: IconMode,
    ) -> Self {
        let names: Vec<String> = match &subset {
            Some(subset) => subset
                .iter()
                .map(|&index| entry_name(&files[index as usize]))
                .collect(),
            None => files.iter().map(entry_name).collect(),
        };
        Self {
            names: IndexedSource::new(names),
            subset,
            emu_files: files,
            width: 70,
            mode: icon_mode,
        }
    }

    fn index(&self, id: usize) -> Option<usize> {
        match &self.subset {
            Some(subset) => subset.get(id).map(|&i| i as usize),
            None => Some(id),
        }
    }

    fn file(&self, id: usize) -> Option<&EmuFile> {
        self.emu_files.get(self.index(id)?)
    }
}

impl FuzzySource<EmuFile> for PickerSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        self.file(id)
            .map(|file| entry_info(file, self.width as usize))
            .unwrap_or_default()
    }

    fn get_data(&self, id: usize) -> Option<EmuFile> {
        self.file(id).cloned()
    }

    fn get_item(&self, id: usize) -> usize {
        self.index(id).unwrap_or(id)
    }

    fn file_index(&self, id: usize) -> Option<usize> {
        self.index(id)
    }
    fn get_icon(&self, id: usize) -> (Option<ListIcon>, Option<ListIcon>) {
        let Some(file) = self.file(id) else {
            return (None, None);
        };
        let p = file.get_meta("platform").split(';').next().unwrap_or("");
        let c = file.get_meta("category").split(';').next().unwrap_or("");
        let category = ListIcon::Glyph(category_icon(c), category_color(c));
        let platform = ListIcon::Glyph(platform_icon(p), platform_color(p));
        match self.mode {
            IconMode::Categories | IconMode::Platforms => (Some(category), Some(platform)),
            IconMode::CategoryOnly => (Some(category), None),
            IconMode::PlatformOnly => (None, Some(platform)),
            IconMode::All => (None, None),
        }
    }
}

fn category_icon(category: &str) -> char {
    // match category {
    //     "Music" => '',
    //     "Graphics" => '',
    //     "Tool" | "Other Platform C64 Tool" => '󱁤',
    //     "Game" | "Game Preview" => '󰊖',
    //     "Crack" => '󰋮',
    //     "Demo" | "Intro" | "4K Intro" | "8K Intro" | "64K Intro" | "40k Intro" | "256b Intro"
    //     | "128b Intro" | "64b Intro" => '󱄄',
    //     "Invitation" => '󰺻',
    //     _ => '󰧯',
    // }
    match category {
        "Music" => '󰺢',
        "Streaming Music" => '',
        "Graphics" => '',
        "Tool" | "Other Platform C64 Tool" => '󱁤',
        "Game" | "Game Preview" => '󰯽',
        "Crack" => '󰯱',
        "Intro" => '󰰃',
        "Demo" => '󰯴',
        "4K Intro" => '󰎮',
        "1K Intro" => '󰎦',
        "8K Intro" => '󰎻',
        "32K Intro" => '󰰃',
        "64K Intro" => '󰰃',
        "40k Intro" => '󰰃',
        "256b Intro" => '󰎩',
        "128b Intro" | "64b Intro" => '󰰃',
        "Invitation" => '󰰪',
        _ => '󰗮',
    }
}

fn category_color(category: &str) -> u32 {
    match category {
        "Music" => 0x40c0f0,
        "Graphics" => 0xf0c0f0,
        "Demo" => 0x80fffe,
        "Intro" => 0xa06060,
        "4K Intro" => 0x60e060,
        "8K Intro" => 0x60e0e0,
        "64K Intro" => 0xb07070,
        "40K Intro" => 0xb08050,
        "Crack" => 0x808080,
        "256b Intro" | "128b Intro" | "64b Intro" => 0x905090,
        "Invitation" => 0xe0e050,
        _ => 0xff00ff00,
    }
}

fn platform_icon(platform: &str) -> char {
    match platform {
        "Windows" => '',
        "MS-Dos" => '',
        "Atari 2600" | "SNES" | "Neo Geo" => '',
        "ZX Spectrum" => '󰨛',
        "Gameboy" | "GBA" | "Lynx" => '󱎓',
        "Amiga" | "Amiga AGA" | "Atari ST" => '󰉉',
        "C16" | "Amstrad CPC" | "C64" | "Atari XL" => '󰧯',
        "PlayStation" | "PSP" | "Megadrive" => '󰊖',
        _ => ' ',
    }
}

fn platform_color(platform: &str) -> u32 {
    match platform {
        "Windows" => 0x40c0f0,
        "MS-Dos" => 0xe09090,
        "Atari 2600" | "SNES" | "Gameboy" | "GBA" => 0xa0a0a0,
        "ZX Spectrum" => 0xe0e030,
        "Amiga" | "Amiga AGA" | "C64" => 0xe0e070,
        "Atari ST" | "Atari XL" => 0xe08080,
        "C16" => 0x906060,
        "Amstrad CPC" => 0x40a070,
        "PlayStation" | "PSP" | "Megadrive" => 0x20df30,
        _ => 0xff00ff00,
    }
}

#[derive(Clone)]
pub struct DownloadSource {
    names: IndexedSource,
    downloads: Vec<&'static str>,
    emu_file: EmuFile,
    width: u32,
}

impl DownloadSource {
    pub fn new(emu_file: &EmuFile) -> Self {
        let downloads: Vec<&'static str> = emu_file.get_meta("download").split(";").collect();
        let names: Vec<String> = emu_file
            .get_meta("download")
            .split(";")
            .map(|s| s.rsplit('/').next().unwrap_or(s))
            .map(|s| s.to_string())
            .collect();
        Self {
            names: IndexedSource::new(names),
            downloads,
            emu_file: emu_file.clone(),
            width: 72,
        }
    }
}

impl FuzzySource<EmuFile> for DownloadSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        let mut emu_file = self.emu_file.clone();
        emu_file.meta.insert("download", self.downloads[id]);
        emu_file.path = FileSource::Url(UrlList::one(self.downloads[id]));
        entry_info(&emu_file, self.width as usize)
    }

    fn get_data(&self, id: usize) -> Option<EmuFile> {
        let mut emu_file = self.emu_file.clone();
        emu_file.meta.insert("download", self.downloads[id]);
        emu_file.path = FileSource::Url(UrlList::one(self.downloads[id]));
        Some(emu_file)
    }

    fn get_item(&self, id: usize) -> usize {
        id
    }
}

/// Shorten `url` to at most `max` characters by dropping path components from
/// the left, keeping the two parts that identify it — the host it came from and
/// the file name at the end. Everything dropped is replaced by a single `...`:
///
/// `https://ftp.example.org/pub/demos/c64/1992/zentro4.zip`
/// → `https://ftp.example.org/.../1992/zentro4.zip`
/// → `https://ftp.example.org/.../zentro4.zip`
///
/// A URL still too long once every component is gone has nothing left to drop,
/// so it is cut out of the middle instead, keeping its head and the end of the
/// file name (extension included).
fn trunc_url(url: &str, max: usize) -> String {
    if url.chars().count() <= max {
        return url.to_string();
    }

    // Split into `scheme://host` and the path below it. The path search starts
    // after `://` so the scheme's own slashes don't count as the first one.
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    let (host, path) = match url[after_scheme..].find('/') {
        Some(i) => url.split_at(after_scheme + i),
        // No path at all: there is nothing to drop, only the middle cut below.
        None => (url, ""),
    };
    let (dirs, file) = match path.rsplit_once('/') {
        Some((dirs, file)) => (dirs.trim_start_matches('/'), file),
        None => ("", ""),
    };
    let dirs: Vec<&str> = if dirs.is_empty() {
        Vec::new()
    } else {
        dirs.split('/').collect()
    };

    // Drop one more leading component per round until what's left fits.
    for skip in 1..=dirs.len() {
        let kept = dirs[skip..].join("/");
        let candidate = if kept.is_empty() {
            format!("{host}/.../{file}")
        } else {
            format!("{host}/.../{kept}/{file}")
        };
        if candidate.chars().count() <= max {
            return candidate;
        }
    }

    middle_cut(&format!("{host}/.../{file}"), max)
}

/// Cut `s` down to `max` characters by removing from the middle, so both ends
/// stay readable. Used as [`trunc_url`]'s last resort.
fn middle_cut(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return chars.iter().take(max).collect();
    }
    let keep = max - 3;
    let front = keep / 2;
    let back = keep - front;
    let head: String = chars[..front].iter().collect();
    let tail: String = chars[chars.len() - back..].iter().collect();
    format!("{head}...{tail}")
}

/// The single line an entry gets in the picker list: `title / group`.
fn entry_name(file: &EmuFile) -> String {
    let info = &file.game_info;
    if info.title.is_empty() {
        "???".into()
    } else if info.group.is_empty() {
        info.title.to_string()
    } else {
        format!("{} / {}", info.title, info.group)
    }
}

/// Everything we know about an entry, for the picker's info field: title,
/// group, what it is and when, the party it was released at, its tags, and
/// where it comes from. Empty fields are left out rather than shown blank.
fn entry_info(file: &EmuFile, width: usize) -> String {
    let mut lines = Vec::new();
    let platform = file.get_meta("platform");
    let category = file.get_meta("category");
    let year = file.game_info.year();
    let year = if year == 0 {
        "".to_string()
    } else {
        format!(" ({year})")
    };
    if platform.is_empty() {
        lines.push(format!("{category}{year}"));
    } else {
        lines.push(format!("{platform} {category}{year}"));
    }
    if let Some(party) = file.meta.get("party").filter(|p| !p.is_empty()) {
        lines.push(format!("Party: {party}"));
    }
    if let Some(tags) = file.meta.get("tags").filter(|t| !t.is_empty()) {
        lines.push(format!("Tags: {tags}"));
    }

    let source = match &file.path {
        FileSource::Path(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()),
        FileSource::Url(urls) => urls
            .first()
            .map(|u| trunc_url(u, width))
            .unwrap_or_default(),
    };
    if !source.is_empty() {
        lines.push(source);
    }
    lines.join("\n")
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
                writer.write(SetHudText {
                    text: emu.get_info(),
                    duration: Duration::from_secs(2),
                    location: HudLocation::InfoText,
                    ..Default::default()
                });
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
                    Cmd::Reload => {
                        settings.current_game -= 1;
                        if let Some(launch) = navigator.current_launch.as_mut() {
                            launch.index -= 1;
                        }
                        emu.run_next = true;
                    }
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
                    Cmd::Reset => {
                        emu.reset();
                    }
                    Cmd::AddToPlaylist if emu.core.is_some() => {
                        let picker = Arc::new(PlaylistPicker::new(&playlists, &emu.emu_file));
                        show_list.write(ShowFuzzyList {
                            id: PLAYLIST_PICKER,
                            source: picker.clone(),
                            prompt: Some(String::new()),
                            selected: None,
                            title: "Add to playlist".into(),
                        });
                        commands.insert_resource(PlaylistPick {
                            file: emu.emu_file.clone(),
                            fetched: emu.work_file.get_meta_or("fetched", ""),
                            picker,
                        });
                    }
                    Cmd::ToggleInfo => {
                        if emu.show_info {
                            writer.write(SetHudText {
                                location: HudLocation::InfoText,
                                ..Default::default()
                            });
                        } else {
                            writer.write(SetHudText {
                                text: emu.get_info(),
                                delay: Duration::from_secs(0),
                                duration: Duration::from_secs(5000),
                                location: HudLocation::InfoText,
                            });
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
                        writer.write(SetHudText {
                            text: emu.get_info(),
                            duration: Duration::from_secs(5000),
                            location: HudLocation::InfoText,
                            ..Default::default()
                        });
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
                    handle_textlist,
                    handle_playlist_pick,
                    handle_cmd.run_if(on_message::<CmdMessage>),
                ),
            );
    }
}

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
