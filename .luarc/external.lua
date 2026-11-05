---@meta
-- Globals for --remote-control scripts, registered in src/remote_control.rs.

---An entry from a db or playlist: the db's fields, plus `id`.
---@class Demo
---@field id string "demozoo.1234", "csdb.5678" or a bare id
---@field title? string
---@field [string] string

---Keys demarc can deliver to the emulator.
---@enum KeyName
Key = {
	Backspace = "Backspace",
	Tab = "Tab",
	Enter = "Enter",
	Pause = "Pause",
	Escape = "Escape",
	Space = "Space",
	Quote = "Quote",
	Comma = "Comma",
	Minus = "Minus",
	Period = "Period",
	Slash = "Slash",
	Digit0 = "Digit0",
	Digit1 = "Digit1",
	Digit2 = "Digit2",
	Digit3 = "Digit3",
	Digit4 = "Digit4",
	Digit5 = "Digit5",
	Digit6 = "Digit6",
	Digit7 = "Digit7",
	Digit8 = "Digit8",
	Digit9 = "Digit9",
	Semicolon = "Semicolon",
	Equal = "Equal",
	BracketLeft = "BracketLeft",
	Backslash = "Backslash",
	BracketRight = "BracketRight",
	Backquote = "Backquote",
	KeyA = "KeyA",
	KeyB = "KeyB",
	KeyC = "KeyC",
	KeyD = "KeyD",
	KeyE = "KeyE",
	KeyF = "KeyF",
	KeyG = "KeyG",
	KeyH = "KeyH",
	KeyI = "KeyI",
	KeyJ = "KeyJ",
	KeyK = "KeyK",
	KeyL = "KeyL",
	KeyM = "KeyM",
	KeyN = "KeyN",
	KeyO = "KeyO",
	KeyP = "KeyP",
	KeyQ = "KeyQ",
	KeyR = "KeyR",
	KeyS = "KeyS",
	KeyT = "KeyT",
	KeyU = "KeyU",
	KeyV = "KeyV",
	KeyW = "KeyW",
	KeyX = "KeyX",
	KeyY = "KeyY",
	KeyZ = "KeyZ",
	Delete = "Delete",
	Numpad0 = "Numpad0",
	Numpad1 = "Numpad1",
	Numpad2 = "Numpad2",
	Numpad3 = "Numpad3",
	Numpad4 = "Numpad4",
	Numpad5 = "Numpad5",
	Numpad6 = "Numpad6",
	Numpad7 = "Numpad7",
	Numpad8 = "Numpad8",
	Numpad9 = "Numpad9",
	NumpadDecimal = "NumpadDecimal",
	NumpadDivide = "NumpadDivide",
	NumpadMultiply = "NumpadMultiply",
	NumpadSubtract = "NumpadSubtract",
	NumpadAdd = "NumpadAdd",
	NumpadEnter = "NumpadEnter",
	NumpadEqual = "NumpadEqual",
	ArrowUp = "ArrowUp",
	ArrowDown = "ArrowDown",
	ArrowRight = "ArrowRight",
	ArrowLeft = "ArrowLeft",
	Insert = "Insert",
	Home = "Home",
	End = "End",
	PageUp = "PageUp",
	PageDown = "PageDown",
	F1 = "F1",
	F2 = "F2",
	F3 = "F3",
	F4 = "F4",
	F5 = "F5",
	F6 = "F6",
	F7 = "F7",
	F8 = "F8",
	F9 = "F9",
	F10 = "F10",
	F11 = "F11",
	F12 = "F12",
	F13 = "F13",
	F14 = "F14",
	F15 = "F15",
	NumLock = "NumLock",
	CapsLock = "CapsLock",
	ScrollLock = "ScrollLock",
	ShiftRight = "ShiftRight",
	ShiftLeft = "ShiftLeft",
	ControlRight = "ControlRight",
	ControlLeft = "ControlLeft",
	AltRight = "AltRight",
	AltLeft = "AltLeft",
	SuperLeft = "SuperLeft",
	SuperRight = "SuperRight",
	Help = "Help",
	PrintScreen = "PrintScreen",
	ContextMenu = "ContextMenu",
	Power = "Power",
	Undo = "Undo",
	BrowserBack = "BrowserBack",
	BrowserForward = "BrowserForward",
	BrowserRefresh = "BrowserRefresh",
	BrowserStop = "BrowserStop",
	BrowserSearch = "BrowserSearch",
	BrowserFavorites = "BrowserFavorites",
	BrowserHome = "BrowserHome",
	AudioVolumeMute = "AudioVolumeMute",
	AudioVolumeDown = "AudioVolumeDown",
	AudioVolumeUp = "AudioVolumeUp",
	MediaTrackNext = "MediaTrackNext",
	MediaTrackPrevious = "MediaTrackPrevious",
	MediaStop = "MediaStop",
	MediaPlayPause = "MediaPlayPause",
	LaunchMail = "LaunchMail",
}

---Hotkey commands, for `send_cmd`.
---@enum CmdName
Cmd = {
	NextFile = "NextFile",
	PrevFile = "PrevFile",
	SwapDisk = "SwapDisk",
	ChangeScale = "ChangeScale",
	ToggleCrt = "ToggleCrt",
	ToggleBorder = "ToggleBorder",
	PauseResume = "PauseResume",
	MouseClick = "MouseClick",
	ToggleInput = "ToggleInput",
	ToggleInfo = "ToggleInfo",
	Reset = "Reset",
	Screenshot = "Screenshot",
	ScreenshotAll = "ScreenshotAll",
	Warp10 = "Warp10",
	Warp30 = "Warp30",
	Fullscreen = "Fullscreen",
	ToggleAll = "ToggleAll",
	NextEmu = "NextEmu",
	PrevEmu = "PrevEmu",
	Maximize = "Maximize",
	NextFileAll = "NextFileAll",
	OpenFile = "OpenFile",
	Reload = "Reload",
	Settings = "Settings",
	ShaderDialog = "ShaderDialog",
	StartOther = "StartOther",
	AddToPlaylist = "AddToPlaylist",
	OpenKeyboard = "OpenKeyboard",
	ToggleRecording = "ToggleRecording",
	ToggleMotion = "ToggleMotion",
	NextSong = "NextSong",
	PrevSong = "PrevSong",
	SeekFwd30 = "SeekFwd30",
	SeekBack30 = "SeekBack30",
	SeekFwd10m = "SeekFwd10m",
	SeekBack10m = "SeekBack10m",
	ZoomIn = "ZoomIn",
	ZoomOut = "ZoomOut",
	ScrollUp = "ScrollUp",
	ScrollDown = "ScrollDown",
	ScrollLeft = "ScrollLeft",
	ScrollRight = "ScrollRight",
	NextSubtitle = "NextSubtitle",
}

---Suspend the script for `n` frames.
---@param n integer
function wait_frames(n) end

---Hold a key down, then release it.
---@param key KeyName
---@param frames? integer Frames to hold the key, default 2
function press_key(key, frames) end

---Press a key and keep it held until `key_up`.
---@param key KeyName
function key_down(key) end

---Release a key pressed with `key_down`.
---@param key KeyName
function key_up(key) end

---Run a hotkey command, as if its key was pressed.
---@param cmd CmdName
function send_cmd(cmd) end

---Lands a few frames later; `quit()` waits for it.
---@param path string
function screenshot(path) end

---RMS (0..1) of what the cores played since it was last asked.
---@return number
function audio_level() end

---Write all audio from now on to a wav file.
---@param path string
function record_audio(path) end

---Share (0..1) of the current frame that isn't black, and a hash that tells frames apart.
---@return number level
---@return integer hash
function video_level() end

---Start an entry from the dbs; follow it with `load_status()`.
---@param id string|integer "demozoo.1234", "csdb.5678", or a bare id in the first db
function load_demo(id) end

---State of the current emulator. `error` is set when it is "failed".
---@return "loading"|"running"|"idle"|"failed" status
---@return string? error
function load_status() end

---Entries at a navigator path like "Demozoo/Parties/Evoke 2005/Demo"; nil if there is no such path.
---@param path string
---@return Demo[]?
function get_navigator(path) end

---Every entry in the folders below a path; "*" in a segment matches any text.
---@param pattern string
---@return Demo[]?
function collect_navigator(pattern) end

---Entries of a playlist; nil if there is no such playlist.
---@param name string
---@return Demo[]?
function get_playlist(name) end

---Add an entry to a playlist, creating it if needed.
---@param name string
---@param id string
function save_to_playlist(name, id) end

---Remove an entry from a playlist.
---@param name string
---@param id string
function remove_from_playlist(name, id) end

---Send the log and the cores' output to a file; `print` still goes to the terminal.
---@param path string
function log_to_file(path) end

---Exit demarc once pending screenshots are written.
function quit() end
