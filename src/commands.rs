use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::Duration;

use bevy::prelude::*;
use bevy::window::{PrimaryWindow, WindowMode};
use percent_encoding::percent_decode_str;
use url::Url;

use crate::egui_ui::HudLocation;
use crate::egui_ui::{FuzzyListSelect, HudState, SetHudText, ShowFuzzyList};
use crate::emulator::{Emulator, InputMode};
use crate::fuzzy_list::AllWordsSource;
use crate::fuzzy_list::{FuzzySource, IndexedSource};
use crate::media_keys::{self, MediaKeyEvent, MediaKeyInfo};
use crate::post_process::{BorderMode, ScaleMode};
use crate::{AppSettings, RenderSettings};
use crate::{EmuFile, emu_file::FileSource};

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
}

#[derive(Message)]
pub struct CmdMessage(pub Cmd);

/// Id the file picker is opened under, echoed back by
/// [`FuzzyListSelect`] so its selections are told apart from any other list's.
pub const FILE_PICKER_ID: usize = 1;

/// Id of the second list, over one entry's download URLs, that Shift+Enter in
/// the file picker opens (see [`handle_textlist`]).
pub const DOWNLOAD_PICKER_ID: usize = 2;

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

fn handle_textlist(
    mut settings: ResMut<AppSettings>,
    input: Res<ButtonInput<KeyCode>>,
    mut file_reader: MessageReader<FuzzyListSelect>,
    mut writer: MessageWriter<CmdMessage>,
    mut show_list: MessageWriter<ShowFuzzyList>,
    time: Res<Time>,
    hud: Res<HudState>,
    // The entry whose downloads the list opened by Shift+Enter is showing, kept
    // until that list reports back (its own `item` is a URL index, not a file).
    mut download_pick: Local<Option<usize>>,
) {
    // The file picker is the egui list in `crate::egui_ui`, which closes itself
    // once a row is picked; `item` is the stable index into `settings.files`,
    // independent of the current search filter.
    for &FuzzyListSelect { id, item, alt, .. } in file_reader.read() {
        info!("Got SELECT {id} {item:?} alt={alt}");
        match id {
            FILE_PICKER_ID => {
                // Shift+Enter picks the download instead of starting it: an
                // entry's URLs are alternatives (a mirror, the same release
                // packed differently), and a plain load takes whichever of them
                // answers first. A second list over their file names lets the
                // user say which one to use.
                let picker = alt
                    .then(|| original_file(&settings, item).and_then(DownloadSource::new))
                    .flatten();
                if let Some(source) = picker {
                    *download_pick = Some(item);
                    show_list.write(ShowFuzzyList {
                        id: DOWNLOAD_PICKER_ID,
                        source: Arc::new(source),
                    });
                    continue;
                }
                settings.current_game = item as isize;
                writer.write(CmdMessage(Cmd::Reload));
            }
            DOWNLOAD_PICKER_ID => {
                let Some(file) = download_pick.take() else {
                    continue;
                };
                // Narrow the entry down to the one URL, so the load fetches
                // that and nothing else -- `FileSource::resolve` would
                // otherwise re-apply its own idea of which of them to take.
                // The picker's snapshot still holds them all, so the entry can
                // be pointed at a different download later.
                let url = original_file(&settings, file)
                    .and_then(download_urls)
                    .and_then(|urls| urls.get(item).cloned());
                if let Some(url) = url {
                    settings.files[file].path = FileSource::Url(vec![url]);
                }
                settings.current_game = file as isize;
                writer.write(CmdMessage(Cmd::Reload));
            }
            _ => {
                if item < HOTKEYS.len() {
                    let cmd = HOTKEYS[item].cmd;
                    writer.write(CmdMessage(cmd));
                }
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
        // TODO: We sometimes get quick PRESS/RELEASE/PRESS for only press
        let modal = hud.list_open();
        if modal {
            return;
        }
        if time.elapsed_secs() - settings.hotkey_pressed < 0.35 {
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
            });
        }
    }
}

/// The entry as the picker first saw it. [`FilePickerSource`] snapshots
/// `settings.files` when the picker is first built, so an entry that has since
/// been narrowed to a single download still has all of its URLs here — and can
/// be pointed at another one of them.
fn original_file(settings: &AppSettings, index: usize) -> Option<&EmuFile> {
    settings
        .file_source
        .as_ref()
        .and_then(|source| source.info.get(index))
        .or_else(|| settings.files.get(index))
}

/// The URLs of an entry there is something to choose between: several remote
/// alternatives. A local path, or a single URL, has nothing to pick from.
fn download_urls(file: &EmuFile) -> Option<&Vec<Url>> {
    match &file.path {
        FileSource::Url(urls) if urls.len() > 1 => Some(urls),
        _ => None,
    }
}

/// The file-name part of `url`, percent-decoded for display: the last path
/// segment, without any `?query` or `#fragment`. A URL ending in a slash has no
/// file name, so it is listed whole.
fn url_file_name(url: &Url) -> String {
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or_default();
    let name = percent_decode_str(name).decode_utf8_lossy();
    if name.is_empty() {
        url.as_str().to_owned()
    } else {
        name.into_owned()
    }
}

/// Backs the download picker: the file-name part of each of one entry's URLs,
/// with the whole URL in the info field below the list — mirrors of the same
/// release often share a file name, and the host is what tells them apart.
///
/// The `id` a selection reports is the index of the URL in the entry's own
/// list, which is how [`handle_textlist`] finds it again.
struct DownloadSource {
    names: AllWordsSource,
    urls: Vec<Url>,
}

impl DownloadSource {
    /// The picker over `file`'s downloads, or `None` when there is nothing to
    /// pick between (see [`download_urls`]).
    fn new(file: &EmuFile) -> Option<Self> {
        let urls = download_urls(file)?;
        let names = urls.iter().map(url_file_name).collect();
        Some(Self {
            names: AllWordsSource::new(names),
            urls: urls.clone(),
        })
    }
}

impl FuzzySource for DownloadSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        self.urls.get(id).map(Url::to_string).unwrap_or_default()
    }
}

/// Backs the file picker: an [`IndexedSource`] over the one-line names shown in
/// the list, paired with the fuller per-entry detail (year, type, party, …)
/// shown in the info field below it.
///
/// Both are built once, on first open, and reused on every open after that —
/// cloning is a pair of `Arc` bumps, not a re-index.
#[derive(Clone)]
pub struct FilePickerSource {
    names: IndexedSource,
    /// Info text per entry, indexed by the same id `names` reports.
    info: Arc<Vec<EmuFile>>,
    width: u32,
}

impl FilePickerSource {
    fn new(files: &[EmuFile]) -> Self {
        let mut names = Vec::with_capacity(files.len());
        let mut info = Vec::with_capacity(files.len());
        for file in files {
            names.push(entry_name(file));
            info.push(file.clone());
        }
        Self {
            names: IndexedSource::new(names),
            info: Arc::new(info),
            width: 70,
        }
    }
}

impl FuzzySource for FilePickerSource {
    fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        self.names.search(query, limit)
    }

    fn get_text(&self, id: usize) -> String {
        self.names.get_text(id)
    }

    fn get_info(&self, id: usize) -> String {
        entry_info(&self.info[id], self.width as usize)
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
        info.title.clone()
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
    let year = file.game_info.year;
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
            .map(|u| trunc_url(u.as_str(), width))
            .unwrap_or_default(),
    };
    if !source.is_empty() {
        lines.push(source);
    }
    lines.join("\n")
}

fn handle_cmd(
    mut cmds: MessageReader<CmdMessage>,
    mut emus: Query<&mut Emulator>,
    mut settings: ResMut<AppSettings>,
    mut render: ResMut<RenderSettings>,
    mut window: Single<&mut Window, With<PrimaryWindow>>,
    time: Res<Time>,
    mut writer: MessageWriter<SetHudText>,
    mut show_list: MessageWriter<ShowFuzzyList>,
) {
    let mut show_info = false;
    let count = emus.iter().count();
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
            Cmd::OpenFile => {
                // Build the trigram index once, on first open, and reuse it on
                // every open after that — `files` never changes, and indexing
                // the whole list is what made reopening the picker slow. The
                // clone below is a cheap `Arc` bump, not a re-index.
                if settings.file_source.is_none() {
                    settings.file_source = Some(FilePickerSource::new(&settings.files));
                }
                // The info field wraps to the width of the list box, which is
                // as wide as the window is tall; this is what the source
                // truncates the (unwrappable) URL line to.
                let size = window.resolution.size();
                settings.file_source.as_mut().unwrap().width = (size.y / 12.0) as u32;

                show_list.write(ShowFuzzyList {
                    id: FILE_PICKER_ID,
                    source: Arc::new(settings.file_source.clone().unwrap()),
                });
            }
            _ => {}
        }
        for (i, mut emu) in &mut emus.iter_mut().enumerate() {
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
            if settings.all_emus || i == settings.current_emu {
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
                        let floppy = emu.work_file.get_meta("system", "").starts_with("C64");
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
                        debug!("{} vs {}", settings.current_game, settings.files.len());
                    }
                    Cmd::PrevFile => {
                        emu.run_prev = true;
                        debug!("{} vs {}", settings.current_game, settings.files.len());
                    }
                    Cmd::Warp10 => {
                        let text = "\u{f0d71}".to_string();
                        emu.skip(10 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            duration: Duration::from_secs(1),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::Warp30 => {
                        let text = "\u{f0d06}".to_string();
                        emu.skip(30 * 50);
                        writer.write(SetHudText {
                            location: HudLocation::TopRight,
                            duration: Duration::from_secs(1),
                            text,
                            ..Default::default()
                        });
                    }
                    Cmd::Screenshot => {
                        let title = emu.work_file.get_meta("title", "shot");
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

pub struct CommandPlugin;

/// How many frames to wait before `--select` opens the picker. The window is
/// still settling on its final size for the first few frames, and the picker's
/// row count and width are derived from that size.
const SELECT_MENU_DELAY: u32 = 5;

/// When `--select` is passed, open the file-open selector once, a few frames in.
fn open_select_menu(
    args: Res<crate::Args>,
    mut writer: MessageWriter<CmdMessage>,
    mut frame: Local<u32>,
) {
    if !args.select {
        return;
    }
    // Saturating, so the counter never wraps back around to the trigger value.
    *frame = frame.saturating_add(1);
    if *frame == SELECT_MENU_DELAY {
        writer.write(CmdMessage(Cmd::OpenFile));
    }
}

impl Plugin for CommandPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<CmdMessage>();
        app.add_systems(Startup, init_media_keys);
        app.add_systems(
            Update,
            (
                open_select_menu,
                handle_textlist,
                handle_media_keys,
                handle_cmd.run_if(on_message::<CmdMessage>),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fuzzy_list::DEFAULT_MAX_RESULTS;

    const URL: &str = "https://ftp.example.org/pub/demos/c64/1992/zentro4.zip";

    #[test]
    fn short_url_is_left_alone() {
        assert_eq!(trunc_url(URL, URL.len()), URL);
        assert_eq!(trunc_url("http://a.org/x.zip", 70), "http://a.org/x.zip");
    }

    #[test]
    fn path_components_drop_from_the_left_until_it_fits() {
        // One character short. Dropping `pub` alone buys nothing (`...` is just
        // as long), so `demos` goes with it — components come off the left
        // until the result actually fits.
        assert_eq!(
            trunc_url(URL, URL.len() - 1),
            "https://ftp.example.org/.../c64/1992/zentro4.zip"
        );
        // Tighter budgets eat further into the path, always from the left…
        assert_eq!(
            trunc_url(URL, 46),
            "https://ftp.example.org/.../1992/zentro4.zip"
        );
        // …down to just the host and the file name.
        assert_eq!(
            trunc_url(URL, 40),
            "https://ftp.example.org/.../zentro4.zip"
        );
    }

    #[test]
    fn every_result_fits_the_budget() {
        for max in 4..URL.len() + 2 {
            let out = trunc_url(URL, max);
            assert!(
                out.chars().count() <= max,
                "{max}: {out:?} is {} chars",
                out.chars().count()
            );
        }
    }

    #[test]
    fn host_and_file_too_long_together_are_cut_in_the_middle() {
        // Nothing left to drop, so both ends are kept and the middle goes.
        let out = trunc_url(URL, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.starts_with("https://"), "{out}");
        assert!(out.ends_with(".zip"), "{out}");
    }

    #[test]
    fn urls_without_a_path_are_still_bounded() {
        let out = trunc_url("https://a-very-long-host-name.example.org", 20);
        assert_eq!(out.chars().count(), 20);
    }

    #[test]
    fn download_rows_are_the_file_name_part_of_the_url() {
        assert_eq!(url_file_name(&Url::parse(URL).unwrap()), "zentro4.zip");
        // Percent escapes are shown as the characters they stand for...
        assert_eq!(
            url_file_name(&Url::parse("https://a.org/d/Count%20Duckula.zip").unwrap()),
            "Count Duckula.zip"
        );
        // ...and a query string is not part of the name.
        assert_eq!(
            url_file_name(&Url::parse("https://a.org/get?id=1").unwrap()),
            "get"
        );
        // Nothing to name: the whole URL is listed instead.
        let dir = "https://a.org/pub/";
        assert_eq!(url_file_name(&Url::parse(dir).unwrap()), dir);
    }

    /// The download picker is only worth opening when the entry really has
    /// alternatives to pick between.
    #[test]
    fn only_entries_with_several_urls_have_downloads_to_pick() {
        let file = |urls: &[&str]| EmuFile {
            path: FileSource::Url(urls.iter().map(|u| Url::parse(u).unwrap()).collect()),
            ..Default::default()
        };
        assert!(DownloadSource::new(&EmuFile::default()).is_none());
        assert!(DownloadSource::new(&file(&[URL])).is_none());

        let source = DownloadSource::new(&file(&[URL, "https://mirror.example/demo.lha"])).unwrap();
        let rows = source.search("", DEFAULT_MAX_RESULTS);
        assert_eq!(
            rows.iter()
                .map(|&id| source.get_text(id))
                .collect::<Vec<_>>(),
            vec!["zentro4.zip", "demo.lha"]
        );
        // The id a row reports is its index into the entry's URLs, and the
        // info field spells the chosen one out in full.
        assert_eq!(source.get_info(rows[1]), "https://mirror.example/demo.lha");
    }

    #[test]
    fn multibyte_urls_are_counted_in_characters() {
        let url = "https://exämple.org/påth/före/filnämn-ÅÄÖ.zip";
        let out = trunc_url(url, 40);
        assert_eq!(out, "https://exämple.org/.../filnämn-ÅÄÖ.zip");
        assert!(out.chars().count() <= 40);
    }
}
