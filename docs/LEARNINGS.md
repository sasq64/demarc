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
