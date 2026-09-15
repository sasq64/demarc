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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use bevy::input::keyboard::{Key, KeyboardInput, NativeKey};
use bevy::input::{ButtonState, InputSystems};
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::PrimaryWindow;
use mlua::{Function, Lua, LuaOptions, StdLib};

use crate::commands::{Cmd, CmdMessage};
use crate::config::Args;
use crate::emulator::Emulator;
use crate::headless::HeadlessTarget;

/// What a Lua call asks the app to do, applied on the next `PreUpdate`.
pub enum Action {
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    Cmd(Cmd),
    Screenshot(PathBuf),
    Quit,
}

/// The queue the Lua closures write to and the Bevy side reads.
#[derive(Default)]
struct RemoteState {
    actions: Vec<Action>,
    error: Option<String>,
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
        register(&lua, &shared)?;

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

/// Register the globals a script drives demarc with. Every closure is `Send`
/// (mlua's `send` feature) and owns a handle to the shared queue rather than
/// borrowing anything it could not outlive.
fn register(lua: &Lua, shared: &Arc<Mutex<RemoteState>>) -> Result<()> {
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

/// Best-effort logical key for an injected press. The egui dialogs read this
/// for text entry; the emulator only looks at `key_code`.
fn logical_key(key: KeyCode) -> Key {
    let name = format!("{key:?}");
    if let Some(letter) = name.strip_prefix("Key")
        && letter.len() == 1
    {
        return Key::Character(letter.to_lowercase().into());
    }
    if let Some(digit) = name.strip_prefix("Digit")
        && digit.len() == 1
    {
        return Key::Character(digit.into());
    }
    match key {
        KeyCode::Enter | KeyCode::NumpadEnter => Key::Enter,
        KeyCode::Space => Key::Space,
        KeyCode::Tab => Key::Tab,
        KeyCode::Escape => Key::Escape,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::ArrowUp => Key::ArrowUp,
        KeyCode::ArrowDown => Key::ArrowDown,
        KeyCode::ArrowLeft => Key::ArrowLeft,
        KeyCode::ArrowRight => Key::ArrowRight,
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Key::Shift,
        KeyCode::ControlLeft | KeyCode::ControlRight => Key::Control,
        KeyCode::AltLeft | KeyCode::AltRight => Key::Alt,
        KeyCode::SuperLeft | KeyCode::SuperRight => Key::Super,
        _ => Key::Unidentified(NativeKey::Unidentified),
    }
}

fn key_message(key: KeyCode, state: ButtonState, window: Entity) -> KeyboardInput {
    KeyboardInput {
        key_code: key,
        logical_key: logical_key(key),
        state,
        text: None,
        repeat: false,
        window,
    }
}

/// `mlua::Lua` is `Send` but not `Sync`, so the runner is a non-send resource
/// rather than a `Resource`.
struct RemoteControl(ScriptRunner);

/// Exclusive because only `World` can insert a non-send resource.
fn init_script(world: &mut World) {
    let Some(path) = world.resource::<Args>().remote_control.clone() else {
        return;
    };
    match ScriptRunner::new(&path) {
        Ok(runner) => world.insert_non_send(RemoteControl(runner)),
        Err(e) => error!("No remote control: {e:#}"),
    }
}

fn run_script(
    remote: Option<NonSendMut<RemoteControl>>,
    mut commands: Commands,
    mut keys: MessageWriter<KeyboardInput>,
    mut cmds: MessageWriter<CmdMessage>,
    mut exit: MessageWriter<AppExit>,
    windows: Query<Entity, With<PrimaryWindow>>,
    headless: Option<Res<HeadlessTarget>>,
) {
    let Some(mut remote) = remote else { return };
    let window = windows.single().unwrap_or(Entity::PLACEHOLDER);
    for action in remote.0.update() {
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
                commands.spawn(shot).observe(save_to_disk(path));
            }
            Action::Quit => {
                exit.write(AppExit::Success);
            }
        }
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
