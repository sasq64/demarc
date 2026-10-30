//! `--remote-control`: a Luau script that drives demarc.
//!
//! The script's `Main()` runs as a coroutine, resumed once per frame. Its Lua
//! closures are `'static + Send` and cannot reach the Bevy `World`, so they
//! push [`Action`]s onto a shared queue that [`run_script`] drains and applies
//! -- the same split `music_vis` uses to reach the audio thread.
//!
//! `wait_frames` and `press_key` have to yield, which a Rust closure cannot do
//! across the FFI boundary, so they live in [`BOOTSTRAP`] instead.

use std::collections::HashMap;
use std::fs::{self, File};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use bevy::input::keyboard::KeyboardInput;
use bevy::input::{ButtonState, InputSystems};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured, save_to_disk};
use bevy::window::PrimaryWindow;
use mlua::{Function, IntoLuaMulti, Lua, LuaOptions, MultiValue, StdLib};

use crate::commands::{Cmd, CmdMessage};
use crate::config::{AppSettings, Args};
use crate::emu_file::{DemoId, EmuFile};
use crate::emulator::{EmuState, Emulator};
use crate::frontend::EmuView;
use crate::headless::HeadlessTarget;
use crate::loading::LoadFile;
use crate::navigator::Navigator;
use crate::playlists::{Playlists, entry_id};
use crate::ui::key_message;

/// Frames `quit()` gives outstanding screenshots to land.
const QUIT_GRACE_FRAMES: u32 = 120;
/// A channel above this counts as lit, so a dark grey "black" stays black.
const BLACK_LEVEL: u8 = 16;

/// What a Lua call asks the app to do, applied on the next `PreUpdate`.
pub enum Action {
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    Cmd(Cmd),
    Screenshot(PathBuf),
    /// Start this entry: `demozoo.1234`, `csdb.5678`, or a bare id in the first
    /// db. A script names a release the way the db does, not by where it landed
    /// in the file list.
    LoadDemo(String),
    AskNavigator(String),
    AskPlaylist(String),
    AskVideo,
    AskStatus,
    Playlist {
        name: String,
        id: String,
        add: bool,
    },
    Quit,
}

/// The answer to an `Ask*` action, picked up by the script a frame later.
enum Reply {
    Files(Option<Vec<EmuFile>>),
    Video(f64, u32),
    Status(&'static str, Option<String>),
}

/// The queue the Lua closures write to and the Bevy side reads.
#[derive(Default)]
struct RemoteState {
    actions: Vec<Action>,
    reply: Option<Reply>,
    error: Option<String>,
}

/// What the script hears: the cores' own output, taken before the sink so it
/// is there when headless too.
#[derive(Default)]
pub struct AudioTap {
    sum_sq: f64,
    count: usize,
    record: Option<PathBuf>,
    wav: Option<(File, u32)>,
}

impl AudioTap {
    pub fn push(&mut self, rate: u32, samples: &[f32]) {
        self.sum_sq += samples.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        self.count += samples.len();
        if let Some(path) = self.record.take() {
            match File::create(&path) {
                Ok(file) => self.wav = Some((file, 0)),
                Err(e) => error!("Remote control: {path:?}: {e}"),
            }
        }
        if let Some((file, len)) = &mut self.wav {
            let bytes: Vec<u8> = samples
                .iter()
                .flat_map(|s| ((s * 32768.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes())
                .collect();
            *len += bytes.len() as u32;
            // The header is rewritten every time, so the file is whole
            // whenever the app goes away.
            let res = file
                .seek(SeekFrom::Start(0))
                .and_then(|_| file.write_all(&wav_header(rate, *len)))
                .and_then(|_| file.seek(SeekFrom::End(0)))
                .and_then(|_| file.write_all(&bytes));
            if let Err(e) = res {
                error!("Remote control: recording audio: {e}");
                self.wav = None;
            }
        }
    }

    /// RMS, 0 to 1, of what arrived since the last call.
    fn level(&mut self) -> f64 {
        let rms = if self.count == 0 {
            0.0
        } else {
            (self.sum_sq / self.count as f64).sqrt()
        };
        self.sum_sq = 0.0;
        self.count = 0;
        rms
    }
}

fn wav_header(rate: u32, data_len: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(44);
    h.extend(b"RIFF");
    h.extend((36 + data_len).to_le_bytes());
    h.extend(b"WAVEfmt ");
    h.extend(16u32.to_le_bytes());
    h.extend(1u16.to_le_bytes());
    h.extend(2u16.to_le_bytes());
    h.extend(rate.to_le_bytes());
    h.extend((rate * 4).to_le_bytes());
    h.extend(4u16.to_le_bytes());
    h.extend(16u16.to_le_bytes());
    h.extend(b"data");
    h.extend(data_len.to_le_bytes());
    h
}

/// Run after the script, so `Main` is already defined when `Run` wraps it.
const BOOTSTRAP: &str = r#"
function wait_frames(n)
    while n > 0 do
        coroutine.yield()
        n = n - 1
    end
end

function press_key(key, frames)
    key_down(key)
    wait_frames(frames or 2)
    key_up(key)
end

local function ask(send, ...)
    send(...)
    coroutine.yield()
    return Take_reply()
end

function get_navigator(path)
    return ask(Ask_navigator, path)
end

function get_playlist(name)
    return ask(Ask_playlist, name)
end

function video_level()
    return ask(Ask_video)
end

function load_status()
    return ask(Ask_status)
end

function Run(main_fn)
    Main_cr = coroutine.create(function()
        xpcall(main_fn, function(msg)
            Set_error(debug.traceback(msg, 2))
        end)
    end)
end

function Resume()
    if coroutine.status(Main_cr) ~= "dead" then
        coroutine.resume(Main_cr)
    end
    return coroutine.status(Main_cr)
end
"#;

pub struct ScriptRunner {
    /// Held only to keep the state alive: `resume` references it.
    _lua: Lua,
    resume: Function,
    shared: Arc<Mutex<RemoteState>>,
    tap: Arc<Mutex<AudioTap>>,
    done: bool,
}

impl ScriptRunner {
    pub fn new(path: &Path) -> Result<Self> {
        let source = std::fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;

        // The `music_vis` set plus COROUTINE (the script *is* one) and DEBUG
        // (`debug.traceback`). Luau has no `io`, so a failing script reports
        // through `Set_error` rather than writing to stderr itself.
        let lua = Lua::new_with(
            StdLib::STRING
                | StdLib::TABLE
                | StdLib::MATH
                | StdLib::BIT
                | StdLib::BUFFER
                | StdLib::VECTOR
                | StdLib::UTF8
                | StdLib::OS
                | StdLib::COROUTINE
                | StdLib::DEBUG,
            LuaOptions::default(),
        )
        .context("creating the Lua state")?;

        let shared = Arc::new(Mutex::new(RemoteState::default()));
        let tap = Arc::new(Mutex::new(AudioTap::default()));
        register(&lua, &shared, &tap)?;

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "remote.lua".into());
        lua.load(source)
            .set_name(name)
            .exec()
            .with_context(|| format!("running {path:?}"))?;
        lua.load(BOOTSTRAP)
            .set_name("remote_bootstrap")
            .exec()
            .context("running the remote-control bootstrap")?;

        let globals = lua.globals();
        let main = globals
            .get::<Function>("Main")
            .with_context(|| format!("{path:?} defines no Main() function"))?;
        globals.get::<Function>("Run")?.call::<()>(main)?;
        let resume = globals.get::<Function>("Resume")?;

        Ok(Self {
            _lua: lua,
            resume,
            shared,
            tap,
            done: false,
        })
    }

    /// Resume the script for one frame and take whatever it queued.
    pub fn update(&mut self) -> Vec<Action> {
        if !self.done {
            match self.resume.call::<String>(()) {
                Ok(status) => self.done = status == "dead",
                Err(e) => {
                    error!("Remote control: {e}");
                    self.done = true;
                }
            }
        }
        let mut state = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(msg) = state.error.take() {
            error!("Remote control: {msg}");
        }
        std::mem::take(&mut state.actions)
    }

    fn reply(&self, reply: Reply) {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).reply = Some(reply);
    }

    /// True once `Main()` has returned or failed.
    #[cfg(test)]
    pub fn finished(&self) -> bool {
        self.done
    }
}

fn push(shared: &Arc<Mutex<RemoteState>>, action: Action) {
    shared
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .actions
        .push(action);
}

/// The id a script knows `file` by.
fn script_id(file: &EmuFile) -> String {
    match file.demo_id() {
        Some(DemoId { db: "zoo", id }) => format!("demozoo.{id}"),
        Some(DemoId { db, id }) => format!("{db}.{id}"),
        None => entry_id(file),
    }
}

/// The entry `id` names: `db.number`, or a bare id in the first db.
fn find_demo(settings: &AppSettings, id: &str) -> Option<&'static EmuFile> {
    if let Some(file) = settings.default_db().iter().find(|f| f.get_meta("id") == id) {
        return Some(file);
    }
    let want = id
        .rsplit_once('.')
        .and_then(|(db, n)| DemoId::new(db, n.parse().ok()?));
    let mut all = settings.files.values().flat_map(|files| files.iter());
    all.find(|f| want.is_some() && f.demo_id() == want || entry_id(f) == id)
}

/// The share of `pixels` that isn't black, and a hash to tell frames apart.
fn frame_stats(pixels: &[u32]) -> (f64, u32) {
    let lit = pixels
        .iter()
        .filter(|p| p.to_le_bytes()[..3].iter().any(|c| *c > BLACK_LEVEL))
        .count();
    let mut hasher = DefaultHasher::new();
    pixels.hash(&mut hasher);
    (lit as f64 / pixels.len().max(1) as f64, hasher.finish() as u32)
}

fn file_table(lua: &Lua, file: &EmuFile) -> mlua::Result<mlua::Table> {
    let table = lua.create_table()?;
    for (key, value) in &file.meta {
        table.set(*key, *value)?;
    }
    table.set("id", script_id(file))?;
    Ok(table)
}

/// Register the globals a script drives demarc with. Every closure is `Send`
/// (mlua's `send` feature) and owns a handle to the shared queue rather than
/// borrowing anything it could not outlive.
fn register(lua: &Lua, shared: &Arc<Mutex<RemoteState>>, tap: &Arc<Mutex<AudioTap>>) -> Result<()> {
    let globals = lua.globals();

    // The emulator's map is the set of keys demarc can actually deliver, so it
    // doubles as the list a script may name.
    let keys: Arc<HashMap<String, KeyCode>> = Arc::new(
        Emulator::build_keycode_map()
            .keys()
            .map(|k| (format!("{k:?}"), *k))
            .collect(),
    );

    let key_table = lua.create_table()?;
    for name in keys.keys() {
        key_table.set(name.as_str(), name.as_str())?;
    }
    globals.set("Key", key_table)?;

    let cmd_table = lua.create_table()?;
    for cmd in Cmd::ALL {
        cmd_table.set(format!("{cmd:?}"), format!("{cmd:?}"))?;
    }
    globals.set("Cmd", cmd_table)?;

    for (name, down) in [("key_down", true), ("key_up", false)] {
        let state = shared.clone();
        let keys = keys.clone();
        globals.set(
            name,
            lua.create_function(move |_, key: String| {
                let code = *keys
                    .get(&key)
                    .ok_or_else(|| mlua::Error::runtime(format!("unknown key {key:?}")))?;
                push(
                    &state,
                    if down {
                        Action::KeyDown(code)
                    } else {
                        Action::KeyUp(code)
                    },
                );
                Ok(())
            })?,
        )?;
    }

    let state = shared.clone();
    globals.set(
        "send_cmd",
        lua.create_function(move |_, name: String| {
            let cmd = Cmd::from_name(&name)
                .ok_or_else(|| mlua::Error::runtime(format!("unknown command {name:?}")))?;
            push(&state, Action::Cmd(cmd));
            Ok(())
        })?,
    )?;

    let state = shared.clone();
    globals.set(
        "screenshot",
        lua.create_function(move |_, path: String| {
            push(&state, Action::Screenshot(path.into()));
            Ok(())
        })?,
    )?;

    let audio = tap.clone();
    globals.set(
        "audio_level",
        lua.create_function(move |_, ()| {
            Ok(audio.lock().unwrap_or_else(|e| e.into_inner()).level())
        })?,
    )?;

    let audio = tap.clone();
    globals.set(
        "record_audio",
        lua.create_function(move |_, path: String| {
            audio.lock().unwrap_or_else(|e| e.into_inner()).record = Some(path.into());
            Ok(())
        })?,
    )?;

    let state = shared.clone();
    globals.set(
        "load_demo",
        lua.create_function(move |_, id: mlua::Value| {
            let id = match id {
                mlua::Value::Integer(n) => n.to_string(),
                mlua::Value::Number(n) => (n as i64).to_string(),
                mlua::Value::String(s) => s.to_str()?.to_owned(),
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "load_demo wants a demo id, got {}",
                        other.type_name()
                    )));
                }
            };
            push(&state, Action::LoadDemo(id));
            Ok(())
        })?,
    )?;

    let asks: [(&str, fn(String) -> Action); 2] = [
        ("Ask_navigator", Action::AskNavigator),
        ("Ask_playlist", Action::AskPlaylist),
    ];
    for (name, action) in asks {
        let state = shared.clone();
        globals.set(
            name,
            lua.create_function(move |_, arg: String| {
                push(&state, action(arg));
                Ok(())
            })?,
        )?;
    }

    let asks: [(&str, fn() -> Action); 2] = [
        ("Ask_video", || Action::AskVideo),
        ("Ask_status", || Action::AskStatus),
    ];
    for (name, action) in asks {
        let state = shared.clone();
        globals.set(
            name,
            lua.create_function(move |_, ()| {
                push(&state, action());
                Ok(())
            })?,
        )?;
    }

    let state = shared.clone();
    globals.set(
        "Take_reply",
        lua.create_function(move |lua, ()| -> mlua::Result<MultiValue> {
            let reply = state.lock().unwrap_or_else(|e| e.into_inner()).reply.take();
            match reply {
                Some(Reply::Files(Some(files))) => {
                    let tables = files.iter().map(|f| file_table(lua, f));
                    let tables = tables.collect::<mlua::Result<Vec<_>>>()?;
                    lua.create_sequence_from(tables)?.into_lua_multi(lua)
                }
                Some(Reply::Video(lit, hash)) => (lit, hash).into_lua_multi(lua),
                Some(Reply::Status(status, error)) => (status, error).into_lua_multi(lua),
                Some(Reply::Files(None)) | None => Ok(MultiValue::new()),
            }
        })?,
    )?;

    for (name, add) in [("save_to_playlist", true), ("remove_from_playlist", false)] {
        let state = shared.clone();
        globals.set(
            name,
            lua.create_function(move |_, (name, id): (String, String)| {
                push(&state, Action::Playlist { name, id, add });
                Ok(())
            })?,
        )?;
    }

    let state = shared.clone();
    globals.set(
        "quit",
        lua.create_function(move |_, ()| {
            push(&state, Action::Quit);
            Ok(())
        })?,
    )?;

    let state = shared.clone();
    globals.set(
        "Set_error",
        lua.create_function(move |_, msg: String| {
            state.lock().unwrap_or_else(|e| e.into_inner()).error = Some(msg);
            Ok(())
        })?,
    )?;

    Ok(())
}

/// `mlua::Lua` is `Send` but not `Sync`, so the runner is a non-send resource
/// rather than a `Resource`.
struct RemoteControl {
    runner: ScriptRunner,
    /// Screenshots asked for and not on disk yet.
    shots: Arc<AtomicUsize>,
    /// Frames left before `quit()` gives up on `shots`.
    quitting: Option<u32>,
}

/// Exclusive because only `World` can insert a non-send resource.
fn init_script(world: &mut World) {
    let Some(path) = world.resource::<Args>().remote_control.clone() else {
        return;
    };
    match ScriptRunner::new(&path) {
        Ok(runner) => {
            let tap = runner.tap.clone();
            crate::emulator::set_audio_tap(move |rate, samples| {
                tap.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(rate, samples);
            });
            world.insert_non_send(RemoteControl {
                runner,
                shots: Arc::default(),
                quitting: None,
            });
        }
        Err(e) => error!("No remote control: {e:#}"),
    }
}

fn run_script(
    remote: Option<NonSendMut<RemoteControl>>,
    mut commands: Commands,
    mut keys: MessageWriter<KeyboardInput>,
    mut cmds: MessageWriter<CmdMessage>,
    mut exit: MessageWriter<AppExit>,
    mut settings: ResMut<AppSettings>,
    mut emus: Query<(&mut Emulator, &EmuView)>,
    mut loads: MessageWriter<LoadFile>,
    mut navigator: ResMut<Navigator>,
    mut playlists: ResMut<Playlists>,
    windows: Query<Entity, With<PrimaryWindow>>,
    headless: Option<Res<HeadlessTarget>>,
) {
    let Some(mut remote) = remote else { return };
    let window = windows.single().unwrap_or(Entity::PLACEHOLDER);
    for action in remote.runner.update() {
        match action {
            Action::KeyDown(key) => {
                keys.write(key_message(key, ButtonState::Pressed, window));
            }
            Action::KeyUp(key) => {
                keys.write(key_message(key, ButtonState::Released, window));
            }
            Action::Cmd(cmd) => {
                cmds.write(CmdMessage(cmd));
            }
            Action::Screenshot(path) => {
                // Asynchronous: the file lands a frame or two later. Written
                // through the `image` crate, whose `png` feature this crate
                // enables even though Bevy's own is off -- don't trim it.
                let shot = match headless.as_deref() {
                    Some(h) => Screenshot::image(h.image.clone()),
                    None => Screenshot::primary_window(),
                };
                if let Some(dir) = path.parent()
                    && let Err(e) = fs::create_dir_all(dir)
                {
                    error!("Remote control: {dir:?}: {e}");
                }
                remote.shots.fetch_add(1, Ordering::Relaxed);
                let shots = remote.shots.clone();
                let mut save = save_to_disk(path);
                commands
                    .spawn(shot)
                    .observe(move |captured: On<ScreenshotCaptured>| {
                        save(captured);
                        shots.fetch_sub(1, Ordering::Relaxed);
                    });
            }
            Action::LoadDemo(id) => {
                let Some(file) = find_demo(&settings, &id) else {
                    error!("Remote control: no demo with id {id}");
                    continue;
                };
                let db = settings.default_db();
                if let Some(index) = db.iter().position(|f| std::ptr::eq(f, file)) {
                    settings.current_game = index as isize;
                }
                // Dropped so the load below replaces the startup load of the
                // db's first entry instead of racing it.
                for (mut emu, view) in &mut emus {
                    if view.index == settings.current_emu {
                        emu.run_next = false;
                        emu.run_prev = false;
                    }
                }
                loads.write(LoadFile {
                    emu_file: file.clone(),
                    target_emulator: Some(settings.current_emu),
                });
            }
            Action::AskNavigator(path) => {
                remote.runner.reply(Reply::Files(navigator.files_at(&path)));
            }
            Action::AskPlaylist(name) => {
                let index = playlists.find(&name);
                let files = index.map(|i| playlists.lists[i].files.to_vec());
                remote.runner.reply(Reply::Files(files));
            }
            Action::AskVideo => {
                let mut stats = (0.0, 0);
                let current = emus.iter().find(|(emu, view)| {
                    !emu.is_crossfade && view.index == settings.current_emu
                });
                if let Some(core) = current.and_then(|(emu, _)| emu.core.as_ref()) {
                    core.with_frame(&mut |width, height, pixels| {
                        stats = frame_stats(&pixels[..width * height]);
                    });
                }
                remote.runner.reply(Reply::Video(stats.0, stats.1));
            }
            Action::AskStatus => {
                let current = emus.iter().find(|(emu, view)| {
                    !emu.is_crossfade && view.index == settings.current_emu
                });
                let (status, error) = match current {
                    Some((emu, _)) => match (&emu.state, &emu.load_error) {
                        (EmuState::Loading, _) => ("loading", None),
                        (_, Some(error)) => ("failed", Some(error.clone())),
                        (EmuState::Stopped, None) => ("idle", None),
                        _ => ("running", None),
                    },
                    None => ("idle", None),
                };
                remote.runner.reply(Reply::Status(status, error));
            }
            Action::Playlist { name, id, add } => {
                let Some(file) = find_demo(&settings, &id) else {
                    error!("Remote control: no demo with id {id}");
                    continue;
                };
                let index = if add {
                    playlists.create(&name)
                } else {
                    playlists.find(&name)
                };
                let Some(index) = index else { continue };
                if playlists.lists[index].contains(file) != add {
                    playlists.toggle(index, file, "");
                }
                let list = &playlists.lists[index];
                navigator.add_playlist(&list.name, list.files);
            }
            Action::Quit => {
                remote.quitting.get_or_insert(QUIT_GRACE_FRAMES);
            }
        }
    }

    let shots = remote.shots.load(Ordering::Relaxed);
    if let Some(frames) = &mut remote.quitting {
        if shots == 0 || *frames == 0 {
            exit.write(AppExit::Success);
        }
        *frames = frames.saturating_sub(1);
    }
}

pub struct RemoteControlPlugin;

impl Plugin for RemoteControlPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, init_script);
        // Before `InputSystems` so an injected key folds into
        // `ButtonInput<KeyCode>` the same frame, reaching both the emulator and
        // the egui dialogs.
        app.add_systems(PreUpdate, run_script.before(InputSystems));
    }
}

#[cfg(test)]
#[path = "tests/remote_control_tests.rs"]
mod tests;
