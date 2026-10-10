# Learnings

- `--headless` has no primary window, so egui never runs: the fuzzy list and
  other dialogs can't be opened or picked from a `--remote-control` script.
  Test picker logic with unit tests on the `FuzzySource` instead.

- A libretro core's own log goes to the `retro` tracing target at *debug*, so
  `RUST_LOG=retro=debug` is what shows why a core refused a file. Reaching for
  it first would have replaced a long guess-and-check hunt through cue
  variations with one line: `CheckCdrom: missing lic sector?`.

- pcsx_rearmed resolves a cue's `FILE` name against the *process working
  directory* before the cue's own directory. A cache-built sheet that keeps the
  track's original name therefore reads the unconverted original whenever demarc
  was started from the release's directory — so a track `prepare_disc` rewrites
  has to be given a name of its own.

- `flamegraph -c "<cmd>"` takes the perf arguments *without* the leading
  `perf`, and appends `-o perf.data` itself, so a second `-o` in there is
  ignored. `-o` on flamegraph names the SVG; perf.data always lands in the cwd.

- perf's dwarf unwinder gets about two frames out of this binary whatever
  `dwarf,<size>` says, so a dwarf flamegraph is flat and useless, and the big
  stack dumps drop 40-55% of samples (varying run to run) into a ring buffer
  capped by `kernel.perf_event_mlock_kb`. `--call-graph fp` plus
  `-C force-frame-pointers=yes` in `.cargo/config.toml` gives mean stack depth
  21 instead of 2, 1 MB of perf.data instead of 400 MB, and no sample loss.

- xdg-open hands an SVG to an image editor, which rasterizes it; an inferno
  flamegraph is only interactive (click to zoom, ctrl-F) in a browser.

- `--speed-test` prints its result with `println!`, which the fd silencing
  swallows: pass `--no-silence` or no fps line appears.

- Never drive a windowed run with `wtype`: it types into whatever has focus, and
  when demarc failed to start the keys went to the user's browser. Check the
  process and window exist first, or stay with `--remote-control`.

- The cross fade takes a load in `load_file` (`cross_fade::redirect_load`), so
  anything that should fade in — or land on the DJ cue — has to arrive as a
  `LoadFile` message; check `git log -S` before blaming the change at hand for
  a regression.

- egui UI can be unit tested on a bare `egui::Context` (the `Harness` in
  `crates/retro-ui/src/tests/lib_tests.rs`), but three things cost a failed run
  each: a new `Area` paints nothing on its first frame; a click needs the
  pointer move in a frame of its own before the press, since egui hit-tests
  against where the pointer was when the frame began; and held modifiers come
  from `Event::ModifiersChanged`, not from the ones stamped on a key event
  (`RawInput` has no `modifiers` field in 0.36).

- `~/.cache/demarc/system/overrides.toml` (and `system/win/*`) is rewritten from
  the copy built into the binary, so editing the cache copy does nothing past
  the next start: edit `system/` and rebuild. Two test runs went by with an old
  override still applied.

- When demarc runs with a stdin that is a pipe nobody closes (an agent's shell)
  it used to block forever reading a "piped db", with an empty log. It skips
  stdin under `--remote-control` now; elsewhere use `</dev/null`.

- `WINEDEBUG=+relay` only traces builtin DLLs, so a native d3dcompiler_47 shows
  nothing. Load the builtin (`d3dcompiler_47=b`) to see `D3DCompile` return
  codes, and set `VKD3D_SHADER_DUMP_PATH=<dir>` to get every shader's source;
  one with a `-source.hlsl` but no `-target.dxbc` is the one that failed.

- A demo's setup dialog can be inspected without a GUI: start it with plain
  wine, then `wine demarc-autodlg.exe --list`.

- A Crinkler intro that loads a DLL with odd casing (`dx8vB.dll`, `WInmm.dll`)
  names it on purpose; `WINEDEBUG=+loaddll` shows the whole import list, which
  the compressed exe does not.

- Plain `rustfmt` on a file reformats lines this repo keeps unformatted; check
  with `cargo check` and leave formatting alone.

- A UPX-packed DLL resolves its imports in its own stub, so a missing export
  shows only as `"X.dll" failed to initialize` (plus a crash in the DETACH that
  follows). `WINEDEBUG=+relay` shows the `GetProcAddress() retval=00000000`. The
  import names are compressed too, so they can't be patched in the file.

- A real DLL copied into the prefix's `syswow64` over wine's fake one is loaded
  without any override, so it applies to every demo — unless wine's own
  builtins import it first: a native `msvcrt.dll` there is never loaded.

- `screenshot()` in a remote-control script is saved asynchronously: a `quit()`
  right after it logs `Failed to send screenshot: sending on a closed channel`
  and writes nothing. `wait_frames(60)` between them.

- demarc only logs the top of a naga error (`Entry point main at Vertex is
  invalid`). `librashader-cli transpile -s <pass.slang> -o vertex -f wgsl`,
  built from the fork, prints the whole cause chain and finds the failing pass
  of a preset without bisecting it.

- A wine demo's GL-drawn launcher (Gaia Machina) is still black in a headless
  screenshot at frame 240 and only drawn by ~360; shoot a few frames apart
  before concluding a window is empty. Try `press_key(Key.Enter)` on a custom
  launcher before building anything — Gaia's takes it as "Run".

- `external/gamescope/build-lr` was configured with `-I<an old session's
  scratchpad>/vk-headers/include` (there is no system `vulkan-headers`), so
  ninja fails on `vulkan/vulkan_core.h` once that is cleaned. Clone
  Vulkan-Headers at the loader's tag (`v1.4.357`) and link it back to the path
  in `meson-info/intro-buildoptions.json` instead of reconfiguring.

- Override `events` frames count core frames, which run ~150 ahead of a remote
  script's `wait_frames`; time clicks against when the target appears in core
  frames, with margin, since wine's startup time varies.

- Manual mouse reaches gamescope as relative motion only, so the cursor drawn
  in the session drifts away from demarc's own; positions logged from demarc's
  cursor then miss when replayed (Tokyo's "resolution" clicks hit Die). Windows
  and web sessions now set `absolute_pointer`, which makes the two agree. A
  click override recorded before that is suspect.

- A wine session can be run without the dialog hidden by adding
  `wine_dialog_res:pick` as a field on the db line given to `--db`.

- On a Steam Deck, wine's `wineusb.sys` (libusb) detaches the kernel driver
  from the built-in controller's USB interface (`3-3:1.2`), so Steam logs
  `Controller device closed after hid_read failure` and every button, Steam
  included, is dead until a reboot. Three wrong causes were named first (wine
  windows on `DISPLAY=:1`, an overwritten binary, winebus hidraw); Steam's
  `logs/controller.txt` and `ls /sys/bus/usb/devices/3-3:1.*/driver` had the
  answer from the start. The prefix sets a `wineusb.sys` DLL override; its
  service `Start`=4 does not stop it loading.

- `pkill -f <pattern>` and `pgrep -f <pattern>` over ssh match the remote shell
  running them, since the pattern is in its own command line: the first killed
  its own session, the second made a wait loop that never ended.

- A Windows demo that shows black or dies at once under a small `wine_res`:
  run it headless with `--no-silence` and grep for `Setting display mode` /
  `Failed to change display mode` before comparing prefixes. A mode larger than
  the session cannot be switched to. Diffing the Deck's prefix against the
  workstation's found only missing wine-mono and cost several rounds.

- The Deck launcher starts in the picker (`--select`), where `handle_gamepad`
  drops pad hotkeys: a pad command meant for use while typing has to be let
  through `ui_state.modal`. Steam's `console_log.txt` logs every
  `ExecuteSteamURL`, which shows whether a press arrived at all.

- The gamescope core keeps its bundled libraries in `lib/` beside the core, and
  a core update restores the `libgudev` SteamOS 3.6 cannot load; every Windows
  demo then fails with a bare `retro_load_game(...) failed`. Running the core
  directory's `./gamescope --version` prints the real symbol error.

- Overrides are keyed on the db's file name (`demozoo.*` → `zoo`, `csdb.*`), so
  a one-line test db called `db.txt` runs without its `overrides.toml` entry.
  Texas started its packed 4k instead of the safe build and "reproduced" a crash
  that was not the one being hunted.

- A message box a demo shows before it has a window is readable from ssh:
  `WINEDEBUG=+relay wine demo.exe 2>&1 | grep 'ret=004'` lists the exe's own
  calls up to the `MessageBoxA`. Texas's "Error" box was a failed
  `SetCurrentDirectoryA("Sample Music")`, not the WMA decoder first suspected.

- A GL demo that is black on Mesa and fine on NVIDIA: run the same headless
  `WINEDEBUG=+opengl` trace on both (`ssh ripper` has the NVIDIA card) and diff
  the call-name histograms before reading single frames. Rupture's showed
  `glLoadProgramNV` on NVIDIA and nothing in its place on Mesa at once; an hour
  went into the Mesa trace alone. Over ssh there, wine is not on `PATH`
  (`~/wine-builds/wine-11.17-amd64-wow64/bin`), and demarc then loads nothing
  and logs nothing.

- fr-063's `Threads[i]->Running==0` assert was "fixed" twice by lowering
  `wine_cpus` (4, then 8), which only made a race rarer: each worker sets
  `Running` itself 5 ms after it is created. A fix for a timing assert needs
  several runs to count as one; the exe is unpacked, so the override patches
  the assert's branch out instead.

- A core option the core never declared is dropped without a word, so a
  `default_meta` entry for one does nothing. Grep the core's option table
  (`core_options.h` in `libretro/dosbox-pure`) before adding a key.

- A DOS demo that cannot find the Sound Blaster DOSBox gives it (Crystal Dream:
  `Could not find output device!`): lower `dosbox_pure_cycles` before trying
  card type, IRQ or GUS. Its detection is a timing loop that runs out at the
  default 200000; six card settings were tried first and all failed alike.
