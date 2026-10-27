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

- dosbox-pure runs real-mode programs on the dynamic core whenever cycles are
  auto-adjusted, and that core has its own FPU decoders (`dyn_fpu_dh.h`,
  `dyn_fpu.h`) beside `fpu.cpp`. Adding FCOMI/FCMOV to `fpu.cpp` alone changed
  nothing for Remnants; a trace line in the new function that never fired showed
  it. A boot sector that sets DOS's COM registers and jumps to the intro under
  `qemu-system-i386` gives a reference picture and, with `-s -S`, a gdb target.

- A core option's value in demarc is the *first* one the core lists, not the
  `default` in its `core_options.h`: `dosbox_pure_midi` reads `disabled` there
  but runs as `frontend`. Print the value in the core before reasoning from the
  header. Futura's crash looked impossible (an IRQ 9 with no MPU-401) until then.
  A guest stack in a core dump shows the way in: `F000:1287` on it is the BIOS
  IRQ 9 stub's `int 0Ah`.

- An unpacked archive sits in `/tmp/demarc-XXXXXX`, so nothing that reads the
  release name off the directory sees the archive's name; it is in the
  `archive_name` meta instead. A rank bonus keyed on the directory name passed
  its unit test (which builds a named directory) and did nothing for the zip.
  Run the real archive headless before calling a picker change done.

- DOS sound that starts and then stops (Crystal Dream, silent after 1.4 s):
  log the DSP commands in `sblaster.cpp` first. One `14 FF FF` and nothing
  after it showed a 64K block nobody restarted; the demo's timer handler polls
  port 22C and restarts only when the DSP reads idle. Cycles, GUS and the mono
  card were tried before that and changed nothing. `audio_level()` and
  `record_audio()` in a remote-control script show where the sound ends;
  `print` there needs `--no-silence`.

- An override `click` used to drive only the libretro pointer, which
  dosbox-pure takes no buttons from; it presses the mouse's left button too
  now. Untitled's setup screen also starts on Enter, which the "try Enter
  first" note above would have found before any code was written.

- A DOS demo that goes black right after setting a banked VESA mode (Into the
  Shadows): log the `int 10h` 4Fxx calls and dump guest memory at the crash
  before trying options. One `4F05` and pixel data running from A0000 up over
  the UMBs showed a blit that expects a #GP at the segment limit. Cycles, GUS
  and memory size were tried first, against the buildbot core and without
  `RUST_LOG=retro=debug`, so the one useful line (`Crash: INT:Gate Selector…`)
  was not even visible. Set `DEMARC_CORE_DIR=libretro/dosbox-pure` from the
  first run.

- "Cmd+<key>" in a request means the app's own hot key, RightAlt/RightCtrl
  held (`handle_hotkey` in `commands.rs`), not the Super/Cmd key. A shortcut
  checked outside `HOTKEYS` has to zero `settings.hotkey_pressed_at`, or the
  hot key's release opens the command list too.

- A one-line db given to `--db` only gets its `overrides.toml` entry when the
  file is named like the db it came from (`demozoo.txt`, `csdb.txt`): the id is
  keyed on the file name, so a `spot.txt` ran without its `wine_res`. Autodlg's
  own log lines only show with `--no-silence`.

- GL drawn to the front buffer never reaches the screen in the wine/gamescope
  session: a single-buffered pixel format, or `glDrawBuffer(GL_FRONT)` plus
  `glFlush`, stays black while the music plays. wined3d's ddraw draws that way
  (Grayscale), and an intro that hard-codes a pixel format index can land on a
  single-buffered one (The Explorer's 8; wine's list alternates DB,DB,SB,SB;
  the opengl32 proxy now remaps that to a double-buffered format).
  Check `+opengl` for `glDrawBuffer buf 1028` / a bare `wglSetPixelFormat`
  before reading shaders: an hour went into shader compile logs that were fine.
  A VIP 2 "control" only showed its GDI launcher, so it proved nothing about GL.
  A 5 KB test exe builds without mingw: `clang --target=i686-w64-windows-gnu
  -nostdlib -fuse-ld=lld -I<wine>/include/wine/windows -Wl,--entry=_start@0`
  against the `.a` files in `<wine>/lib/wine/i386-windows`, and runs as the
  file given to demarc.

- A GL demo's own "can't init" box after a successful `wglMakeCurrent`: look
  for `glGetIntegerv(0d52..0d57)` before it in `+relay`, and dump the unpacked
  code from `/proc/<pid>/mem` while the box is up (`objdump -b binary
  --adjust-vma=0x400000`). Aesterozoa asks for 24 colour bits and no alpha,
  then quits on `GL_ALPHA_BITS` 0. Wine ranks colour bits before alpha, so
  asking for 8 alpha alone still returned the 24-bit format; the opengl32 shim
  asks for 32+8.

- A white GL intro whose shader Mesa rejects is not always a GLSL version
  problem: Surveillance is NVIDIA-only GLSL (`vec4 - vec2`) and
  `wine_glsl_version` did nothing. `RUST_LOG=retro=warn` prints Mesa's compile
  errors now; read them first. `MESA_SHADER_DUMP_PATH=<dir>` names each shader
  `FS_<hash>.glsl`; a corrected file of the same name under
  `MESA_SHADER_READ_PATH` replaces it. That hash covers constants baked into
  the shader, so a replacement made for Surveillance's 1920x1080 exe never
  applied to its other three; an override `glsl` regex patch does.

- "GLSL 1.20 required" from an unversioned shader (Inflorescence's uniform
  initializers): use `wine_glsl_version=130`, never `120`. The forced version
  also applies to glamor's `#version 130` shaders, so 120 kills Xwayland
  ("X11 I/O error! This is fatal"). `wine_glsl_120_subset` does not cover
  uniform initializers.

- `err:seh:NtRaiseException ... c0000005` at an ntdll address as a demo's only
  log line, with `+loaddll` stopping after kernel32: the fault is wine's import
  walk, not the demo. Glutamat's 1k header overlaps code with the import
  descriptor, so `OriginalFirstThunk` is garbage; Windows falls back to the IAT
  when it lies outside the image, wine reads it. The bytes are code, so no
  `patch` can zero them: autodlg maps a copy of the IAT there in the suspended
  child. `winedbg --file cmds` (`cont`, `info reg`, `bt`) under `gamescope
  --backend headless` showed ESI on the descriptor in one run.

- `libretro/gamescope` can be behind the fork the released core is built from:
  a core built from a stale checkout had no PipeWire capture, so a web demo's
  music played on the user's speakers. `git fetch` and compare with
  `origin/demarc` before building it, and grep the `.so` for `capturing the
  session's audio`. A Chrome page stuck on its loading bar: load it in
  `chromium --headless=new --enable-logging=stderr --log-level=0` to read the
  console. `-x key=value` splits on commas, so a literal `gamescope_command`
  with `--window-size=800,600` is cut there without a word.

- The newest directory in `~/.cache/demarc/downloads` is not the demo just run
  (a cached download keeps its old mtime): `find` it by archive name. Tentura's
  "does not start" was a dialog of resolution push buttons with no start
  button; `autodlg --list` showed it in one step.

- Before writing a `patch` from two copies of a file, compare both against the
  one in the cached download (`~/.cache/demarc/downloads/*/`): the copy named
  "fixed" was the pristine one, so the first patch changed nothing.

- An override is found by the db's *file name* (`demozoo.txt` → `zoo`), so a
  one-line test db has to keep that name or no override applies, silently.

- The `{ frame, click }` line demarc logs is the host cursor, which says nothing
  about where a relative-mouse core (DOSBox Pure) has its own cursor: a replayed
  click lands wherever that one is. `dosbox_pure_mouse_input = "direct"` makes
  the core take the click's position.

- A crash in builtin msvcrt's `memset` on a fresh `new()` block that goes
  away under `+heap` or winedbg (both change heap flags) is the demo
  overrunning an earlier block into wine's free-block header (Akkordarbeit:
  a PCX with one padding byte per row). `+virtual` shows the block running
  past its subheap without changing the layout. `/proc/<pid>/mem` is closed
  by Yama, so a UPX exe is unpacked with a downloaded `upx -d`; repacking the
  fixed one with `upx --best` halves the bsdiff, which goes in
  `system/win/patches/` as a `source` rather than inline base64.

- `NtUserChangeDisplaySettings ... returned -2` is not always a mode larger
  than the session: We Ain't Real asks for 640x480 at 85 Hz, and the session
  lists 640x480 at 59 Hz only, so 60 fails as well and neither `wine_res` nor
  `wine_emulate_modeset` helps. `WINEDEBUG=+system` prints the requested
  `dmDisplayFrequency`; a rate of 0 in the demo's config means "default" and
  works at any session size or refresh.

- A D3D8 demo that shows one flat colour, or a zoomed-in corner, through its
  render-target copy (Nature v2.0): look at the stage-0 `TEXCOORDINDEX` and
  `TEXTURETRANSFORMFLAGS` in force when its XYZRHW quads draw. It left
  camera-space texgen from the 3D scene on, which Windows ignores for
  pre-transformed vertices and DXVK applies. Viewport, mip-chain and LOD-bias
  theories came first; a screenshot showing the logo zoomed, and the bypassed
  scenes broken too, pointed at the quads' texcoords. wined3d's d3d8 has no
  symbols, so break on the device vtable entry (`break *(*(unsigned
  int*)(vtbl+4*n))`) in one `winedbg --file` session: attaching under wow64
  faults the break-in thread, and the process dies on `detach`.

- A test db line taken with `grep -n` starts `306676:id:...`, so no id is
  parsed and no override applies, silently; the first test of a new override
  "failed" that way. And `pkill -f gamescope` matches the shell running it,
  locally as over ssh: it killed this session's shell and another session's
  background run.

- A setup dialog that autodlg never answers may belong to a *child* process
  the demo starts (Adelanto runs `e3d_setup.exe` and waits for it); autodlg
  used to search only the launched pid. Its combo also spelled modes
  `640*480`. After that, two crashes followed that were the exe's own: a
  `+relay` grep for the last `RtlAllocateHeap` before the fault found a 0x800
  log buffer overrun by `GL_EXTENSIONS`, then a `GL_MAX_TEXTURE_SIZE`-squared
  texture (0x30000000 bytes, NULL). `__GL_ExtensionStringVersion` and
  `MESA_EXTENSION_MAX_YEAR` do not shorten wine's extension string.

- winetricks' `d3dcompiler_47` is the Windows 8.1 SDK build (6.3.9600), which
  rejects Windows 10 HLSL such as typed UAV loads of `float4` (`X3676`). A
  D3D11 intro that passes NULL for the error blob then faults on the NULL code
  blob right after `D3DCompile` (One Of Those Days, `zoo.185601`). wine's
  builtin compiler is no workaround: it compiled the shader, but DXVK read past
  the end of its DXBC. A 5 KB test exe that calls `D3DCompile` on the
  `VKD3D_SHADER_DUMP_PATH` source with a given DLL printed the error in one
  step. The prefix now takes 10.0.15063 from lutris' `d3d_extras`.

- A `+relay` trace of a 64k intro runs so slowly that the crash it was meant to
  catch never happens before the timeout (Atlas: 5.8 GB of log, no fault).
  `winedbg --file cmds demo.exe` in the background under `gamescope --backend
  headless`, with a second `demarc-autodlg.exe` (no `--launch`) a few seconds
  later to press the dialog, gives registers, `bt` and `x/Ni` of the unpacked
  code at the fault. A NULL slot among a packed exe's resolved imports
  (`x/24x` on the thunk's table) is a missing export: wine's msvcrt lacks
  `__libm_sse2_sqrt_precise`. Pointing AeDebug at `winedbg --minidump` wrote
  nothing.

- A Java demo (javapackager: `runtime/`, `app/*.cfg`) prints its exceptions
  when `runtime/bin/java.exe -cp app/<jar> <app.mainclass>` is run directly;
  the launcher exe shows nothing. On NVIDIA a GL context is exactly the version
  asked for, and LWJGL loads no function above it, so an OPENRNDR demo asking
  for 3.3 core dies on its first 4.x call (`zoo.202844`). Mesa gives the
  highest core version and was fine. A jar whose central directory lists each
  entry twice (`unzip` calls it a zip bomb) can take a recompressed class in
  place when it is no larger; the bsdiff then stays at 12 KB.

- A GL intro whose `+opengl` trace shows `glUniform*` with `-nan(ind)` and
  frames slowing to seconds: an x87 stack leak. Yama (`zoo.398638`) pushes
  its time twice per frame and pops once; after 8 frames `fild` overflows to
  the indefinite NaN. Windows clears the stack somewhere per frame, so the
  opengl32 proxy now runs `emms` after `wglSwapBuffers`. Its
  `ChangeDisplaySettings ... -2` was a garbage DEVMODE and a red herring.
  Getting the unpacked code: a winedbg `break *addr` set before Crinkler
  unpacks is overwritten, and `break SwapBuffers` finds no symbols under
  wow64; `/proc/<pid>/mem` *is* readable from a Python script that started
  `gamescope ... wine demo.exe` itself (ptrace_scope 1 allows ancestors).

- Wine's `midiOutOpen(0)` opens the host's ALSA sequencer port
  (`PipeWire-System`), outside the session's audio capture and with no synth
  behind it, so a winmm MIDI intro is silent without an error
  (`WINEDEBUG=+midi` lists the ports). The winmm proxy now plays `midiOut*`
  through a DirectMusic synth port with `gm.dls`; loading every GM instrument
  costs about a second at open.

- `cargo fmt -- <file>` still formats the whole crate (and the workspace from
  the root); to format one file use `rustfmt --edition 2024 <file>`.

- "Failed to load X.jpg" for a file that exists (Lobstarrs Attack): the
  statically linked libjpeg spills progressive JPEGs to `tmpfile()`, which the
  old MSVC CRT creates as `\tNN.` in the root of the current drive. With the
  cwd on Z: = `/` that fails with access denied. Found with `+relay` (grep
  `ret=00[4-6]`) between the `CreateFileA` and the `MessageBoxA`. Fixed by an
  override patch raising libjpeg's `DEFAULT_MAX_MEM` (`mov eax,1000000` in
  `jpeg_mem_init`). The d3d
  `state_linepattern_w` fixme was a red herring.

- Run `cargo test --workspace`, not `-p demarc`: a crate whose tests don't
  even compile (retro-ui after an `open()` signature change) stays invisible
  otherwise. The `retro_emu` core tests need the cores in `~/.cache/demarc/cores`,
  so they must name the cores demarc actually fetches (`vice_x64sc`,
  `pcsx_rearmed`).

- Kindercrasher (`zoo.86129`) stuck on its first frame was Crinkler mode 2
  (`docs/CRINKLER.md`): `OpenFile` missing from wine's kernelbase became
  `AccessCheck`. A radeonsi glthread segfault seen first was a red herring that
  only showed in plain wine without demarc's kernelbase proxy; check what a
  spinning 4k is doing (perf on the exe, then the stack frame) before
  chasing the first crash a bare-wine run shows. `pgrep -f <exe>` also matches
  the agent's own shell, whose command line contains the name: use `pgrep -x`
  (comm is cut to 15 chars, `rgba_kindercras`).

- Arise (`zoo.31637`) "No OpenGL 1.2 compatible driver found" was not a
  missing entry point: every `wglGetProcAddress` succeeded. It reads the minor
  version as the digit after `strstr(GL_VERSION, "1.")`, which lands on
  `arch1.1` in Mesa's version string. A version check failing on a 4.6 context
  is worth disassembling from the `glGetString(0x1f02)` return address before
  tracing GL calls.

- 86Box device names in a `.cfg` are the `internal_name` from the source, not
  guesses from the display name: the Sound Blaster 2.0 is `sndcard = sb2.0`
  (`sb2` silently installs nothing, so the demo says it cannot initialise the
  card). Check `src/sound/snd_sb.c` for a card's name and config keys; sb2.0 has
  `base`/`irq` (IRQ 5 by default, which the XT disk adapter also uses) and no
  `dma` key.

- `ButtonInput::clear()` only drops the just-pressed/released sets; the key
  stays held, so a second `press()` of it in a test is not `just_pressed`.
  `release()` it before `clear()`.

- A `.com` in a `platform:Windows` release (early-2000s 4Ks, e.g. Mojo Dreams
  `zoo.3286`) is a DOS dropper: an aPACK stub that writes a PE and starts it
  through `int 21h/4B00`, which only NTVDM did. DOSBox runs it to a black
  screen. Depack it (aPLib-style; the stub's own loop is short enough to
  translate), cut the PE out at its `MZ`, and ship that as an override `patch`
  with `boot`.

- Holani (Lynx) is described as needing no BIOS, but without `lynxboot.img` in
  the system dir it runs at full speed with a black screen and no audio. A
  black headless screenshot means nothing on its own; check `audio_level()`
  and compare against a system known to capture (`testdata/gb`).

- The hatari core source is `libretro/hatari` (branch `demarc`). The cached
  core can be older than it: one that ignored machine/CPU/RAM options was
  "fixed" again in a scratch clone of the fork's default branch before the
  `demarc` branch turned out to have it already. Build that branch and point
  `DEMARC_CORE_DIR` at it before patching the core.

- A db line's `download` must be an http(s) URL: a bare path is "unparseable",
  `file://` fails in the fetcher. Serve a local file with
  `python3 -m http.server` to test a db entry.

- Plastic's "Your Fingers So Gently On My Skin" (`zoo.8644`) faulted in the
  CRT's `fread` on a NULL `FILE*`: its own loader, not libjpeg, uses
  `tmpfile()`, so the libjpeg patch does nothing there. The sandbox now
  overlays the release directory and maps it to a free drive letter, so the
  drive root is writable and the files are thrown away. Killing wine skips
  delete-on-close, so a run in the real directory leaves `t6o.*` behind.
  After that it died on NVIDIA with a GLX `BadMatch` in `MakeContextCurrent`:
  wine's `wglBindTexImageARB` copies the pbuffer with its internal context,
  which has a different pixel format; the opengl32 proxy copies with a
  context made on the pbuffer's DC instead. Plain wine opens the demo on the
  user's screen; use demarc headless.

- A silent exit right after a demo's own mode screen (Monstra Kosa,
  `zoo.4364`): `+relay` showed its launcher re-running itself with
  `ShellExecuteA("D:\\", "1")`, built from argv[0], which autodlg passed as a
  Unix path. autodlg now launches with wine's DOS path and waits for the demo's
  child processes before reporting `exited`. An SVKP-protected exe shows no
  strings; a relay of a demarc run is readable by `tail -F` on
  `/tmp/demarc-wine-*.log` while it runs (it is deleted at the end).

- A zip whose entry names use `\` (4k in 5k) unpacked to files with a
  backslash in their name, which wine cannot open ("could not start"). The
  unpacker now splits names on `\`.

- On NVIDIA a GL demo's shader error only shows in its own message box: a
  `+relay` grep for `MessageBoxA` gave fr-080's `C1115 pow(vec4, float)`.
  Its shaders are 2011 ATI-lax GLSL that neither NVIDIA nor Mesa accepts, and
  Its sources can be read from `/proc/<pid>/mem` of a gamescope+wine run
  started by a Python script, but `WINEDEBUG=+msgbox` prints the box's text
  without a relay. Fixed with an override `glsl` patch (opengl32 proxy).
  A GUI demo started by autodlg has no std handles, so the proxy logs with
  ntdll's `__wine_dbg_output`; `NtWriteFile` to `hStdError` printed nothing.

- A Crinkler 4k tagged `opencl` (Shoal) needs an OpenCL ICD on the host
  (`/etc/OpenCL/vendors`); wine's `opencl.dll` loads and unloads at once
  without one and the intro stays black and silent.

- The opengl32 proxy is i386 only, so an override `glsl` patch does nothing for
  a 64-bit demo: no `opengl32: shader` lines at all. Run `file` on whatever the
  launcher starts before converting a fix to `glsl` (Tempus and Umbra launch
  an x86-64 `demo.dat`).

- Testing `wine_cnc_ddraw` on a bare `.exe` (`demarc dir/x.exe`) copies only
  that exe to the temp dir, so its DLLs are missing (`import_dll ... not
  found`). Pass the release directory, or a one-line `demozoo.txt`, instead.
  Dose 2 (`zoo.8540`) cost a run that way.

- A GL crash in `wglBindTexImageARB` (GLX `BadMatch` on
  `X_GLXMakeContextCurrent`) can be a symptom of an earlier failure: Lady Of
  Dark Autumn's `wglChoosePixelFormatARB` for `WGL_TYPE_RGBA_FLOAT_ARB`
  returned no format (Xwayland's GLX has no float fbconfigs), the pbuffer was
  NULL, and the demo then made a NULL context current. `+opengl` traces drop
  lines under demarc, so a `__wine_dbg_output` line in the opengl32 proxy was
  the reliable way to see the current context. After float pbuffers were
  emulated it crashed in its own `fread`: an archive lookup that fails returns
  the name string as the stream ("Demo/glow_h.fp" read as {start, size}).
  The release is ATI-only and takes a missing NV program path when
  `GL_NV_fragment_program` is present; list the archive before chasing heap
  corruption.

- A path-less `No such file or directory` from `demarc::loading` looks like a
  vanished temp dir but was a missing system file (`hatari/tos/tos404.img`,
  never committed) behind a bare `fs::copy(..)?`. Add the path as context first.

- An override `patch` offset names the first byte written, not the
  instruction: Patter's `mov eax,ecx` (8b c1) → `mov eax,esi` changes the
  *second* byte. Writing `c6` over the opcode at the instruction's offset
  decoded as `mov cl,imm8` and crashed later inside the next `imul`
  (`write access to BD0004B4`), which looked like a demo bug for an hour.
  `cmp -l` the patched file against a hand-made copy that is known to work.

- `pkill -f <pattern>` inside a Bash tool call matches the tool's own shell
  (its command line contains the pattern) and kills it: exit 144, no output.
  Kill by PID.

- demarc's wine-log forwarder logs each distinct line once and stops after
  4096 of them, so a `+relay` (or any chatty channel) under demarc shows
  wine's startup and nothing of the demo. Run plain `gamescope --backend
  headless -- wine autodlg.exe --launch demo.exe ...` and log to a file.

- A headless screenshot of a draw-once GDI 4k is black until it has rendered
  *and* the core has stopped hiding: autodlg blocks in `SetWindowPos` on a
  window whose thread never pumps messages (Patter), so the core waits out its
  10 s hide fallback first. Shoot at 25 s, not 10.

- A wine window created at 0x0 with `WS_MAXIMIZE` (Organix, `zoo.51295`,
  `CreateWindowEx("static", 0x97000000, 0,0,0,0)`) never reaches the
  gamescope session: Xwayland makes one buffer-less commit on its surface and
  then none, so the session shows a stale square surface (`client covers
  1024x1024`). The same window created at full size works. Reproduced with a
  test exe (`CreateWindowEx` 0x0 + `WS_MAXIMIZE`, StretchDIBits loop);
  hide/show and resizes after mapping do not help, `_XWAYLAND_ALLOW_COMMITS` is
  not set, no child windows. Root cause not found yet. The demo itself renders
  fine: the DIB dumped from `/proc/<pid>/mem` holds the finished image.

- `err:module:import_dll Library d3dcsx_43.dll ... not found` (The Ink Test,
  `zoo.103547`): wine has no d3dcsx builtin and winetricks no verb, so the
  prefix build takes it from the DirectX June 2010 redist that `d3dx9` already
  downloaded. The import comes from a DLL named `fwdfx.fx`, not the exe.

- A GL 4k that shows its image for an instant and then goes black (Fizzer's
  "The Real Party Is In Your Pocket", `zoo.277164`): no GLX config on
  NVIDIA's Xwayland has an accumulation buffer (`glxinfo -v | grep accum`), so
  every `glAccum` fails silently and only the first swap shows a raw sample.
  The opengl32 proxy now emulates the accumulation buffer with an FBO.

- `DXVK_CONFIG` accepts keys DXVK does not have without a word ("Found config
  env" and "Effective configuration" list them anyway): `d3d9.forceSwapchainMSAA`
  is not a DXVK option and cost two runs. A `grep ... | head` that cut the
  log before the `err:` lines made the first of them look like a fix: grep for
  the error itself before calling something fixed.

- Spilled (`zoo.51354`) asks for `D3DMULTISAMPLE_NONMASKABLE` quality 4. DXVK
  reads that as `1 << quality` = 16 samples, which NVIDIA's Vulkan lacks, so
  `CreateDevice` fails and the intro dereferences NULL. wined3d accepts it but
  multisampled GL points are circles, and its one-point-per-pixel image came
  out tinted with the clear colour. wined3d's `SampleCount=0` (env
  `WINE_D3D_CONFIG`, now the `wine_d3d_config` meta key) turns multisampling off.

- Night Time (`zoo.155141`) presents a D3D11 compute image once and then only
  polls Esc. Under DXVK nothing reaches the session, not even `DXVK_HUD`;
  `d3d11=b;dxgi=b` works. Undecorating was not the cause:
  `-x wine_dialog_res=pick` (autodlg `--no-fill`) was still black.

- On (`zoo.136151`) asserts with `int3` (`0x80000003`) when
  `GL_COMPILE_STATUS` is false; demarc's log shows no GLSL error because the
  driver is NVIDIA, whose log only exists in the intro's memory (`x/s $edi` in
  winedbg at the assert). NVIDIA 610 rejects a `texture()` bias in a vertex
  shader and implicit vec3/vec2-to-float narrowing that older drivers
  accepted; `.x` keeps the old result. Software Mesa failed on a different
  shader first, so its dumps were no help. The kkrunchy-unpacked sources are
  plain strings in the image: one read of `/proc/<pid>/mem` from the parent
  script and glslang over every string found the same faults. An override
  `glsl` `match` is a regex too: unescaped parens silently never match.
  `pkill -f <pattern>` inside a Bash call kills that call's own shell.

- Hologram (`zoo.119170`, same group and engine as On) has the same `int3`
  assert, but its shader sources are not plain strings in memory, so the
  `/proc/<pid>/mem` trick found nothing. Software Mesa with
  `MESA_SHADER_DUMP_PATH` worked instead: add one override patch per error,
  rebuild, rerun, and glslang every dumped file, until nothing fails (the
  intro compiles all 60 shaders at load). Besides vec-to-float narrowing it
  uses default parameter values (`float r=1.1`), which no current compiler
  accepts; one caller relies on the default, so that function gets an
  overload rather than having the default stripped.

- Axxelerate (`zoo.31450`, GLFW 2.7) was black with music on NVIDIA: it
  enables `GL_DEPTH_TEST` once and clears the window's depth only on its
  first frame, and after a swap NVIDIA's window depth no longer passes
  `GL_LEQUAL` for a z=0 quad. The probe that found it, after hours on wrong
  leads: a temporary hook in the opengl32 proxy that `glReadPixels` a row of
  framebuffer 0 after `glEnd` showed frame 1 drawn and frame 2 empty (alpha
  0, so no fragments) with identical state; clearing depth in a `glBegin`
  hook fixed it. Dead ends worth not repeating: a glxinfo FBConfig ID from a
  separate gamescope run does not match the demo's (it "showed" format 33
  single-buffered; a test exe asking `wglGetPixelFormatAttribivARB` said
  double); `ffmpeg -f x11grab` of gamescope's Xwayland sees GDI but not GL,
  so it is no evidence of a black window; demarc's forwarded wine log drops
  repeated lines and stops after 4096 distinct ones, so per-frame calls seem
  absent. GLFW sets its format through gdi32 `SetPixelFormat` and presents
  through gdi32 `SwapBuffers`; both reach wine's builtin opengl32 from a
  win32u callback, so the proxy's `wglSetPixelFormat`/`wglSwapBuffers`
  hooks never see them. wine's `ChoosePixelFormat` treats 0 depth bits as
  "don't care", so a depth-0 request still gets depth 24. A GL test exe's
  window is still black in a screenshot at frame 90; shoot after 400. A Lua
  `screenshot()` right before `quit()` is never written.

- autodlg's `--check Fullscreen` missed dialogs that put the label in a
  `Static` before a textless checkbox (Axxelerate); a textless checkbox now
  takes that label.

- 8K Reality (`zoo.136330`) crashed in `wglUseFontOutlinesA` with garbage
  `format`/`lpgmf`/`deviation`; sanitizing those arguments came first and was
  wrong. The call site pushes four arguments: Crinkler's range import landed
  two exports past `wglUseFontBitmapsA` in wine's table (CRINKLER.md mode 1),
  and the eight popped bytes later let a call overwrite the intro's start time,
  so it exited after one frame. When a Crinkler import gets odd arguments,
  disassemble the call site and map the IAT slots to exports before patching
  the callee.

- A plain `wine` run or a native GL harness without
  `allow_glsl_120_subset_in_110=true` (which demarc sets) fails to compile
  NVIDIA-lax shaders, and Mesa then segfaults linking them; that was chased as
  a Mesa bug for several runs. Run outside demarc with demarc's environment.

- zsh arrays start at 1: `${P[0]}` is empty, so a "pair" test passed an empty
  file name and its crash was the harness's own `fopen`.

- A DOS program that ignores normal key presses but takes held ones (Symbology,
  `zoo.6147`) polls port 64h/60h with IRQ1 unmasked; DOSBox raised IRQ1 at
  once, so the BIOS handler took every scancode first and only typematic
  repeats got through. A CS:IP trace from `retro_run` found the poll loop in
  one run. The core now raises IRQ1 10 µs after the scancode, and not at all
  if the program read it.

- "The recording sounds distorted" turned out to be mpv's own volume boost.
  Before digging into the pipeline, ask what it was played with, or compare
  levels in the file itself (RMS/clip count with numpy) against another player.

- The audio ring runs near `audio_buf_min` (2000-5400 samples, 20-55 ms at
  48 kHz stereo), so any "frontend has stalled" timeout longer than that
  underruns before it fires. The worker steps on its own below
  `audio_buf_min` instead. And a frontend coming back from a stall must not
  catch up on every missed frame: each pull lets the worker push more audio and
  the ring overflows.

- `--headless` opens no audio output, so `AudioOut::is_low` is never true and a
  threaded worker never free-runs: a stalled frontend parks it outright. A stall
  test there leaves gaps between `retro_run`s that a windowed run never has,
  so only a fault that reproduces both ways is real. A busy-wait in the remote
  script (`while os.clock() - t < 0.4 do end`) stalls the main loop.

- A new directory directly under `system/` (here `system/zesarux/`) is not
  packed into the embedded `system.zip` until the build script reruns, so the
  first headless run logged `Unable to open rom file`. `touch
  crates/newsys/build.rs` before building, and check with `unzip -l` on
  `target/<profile>/build/newsys-*/out/system.zip`.

- A wine process is not named after its exe on the host: `pgrep -f name.exe`
  matches the calling shell (its own command line contains the pattern) and
  `pkill -f` on it kills the tool call. Find the demo with
  `ps -eo pid,comm | awk '$2 ~ /^name/'`. VSZ is useless under wow64; sum
  `/proc/<pid>/maps` below 0x7fff0000 for what a 32-bit exe has left.

- "Picture freezes, music plays on" in Ziphead (`zoo.136314`) was not memory:
  I set LAA and `cache_writeback` first, on a guess, and neither helped. What
  found it: a temporary per-second new-frame counter in the gamescope core's
  `retro_run` (stalls of 0-2 fps for seconds), then the render thread's
  voluntary context switches (150/s smooth, 20-40k/s stalled) and the futex
  address in `/proc/<pid>/task/<tid>/syscall`, mapped through `/proc/<pid>/maps`
  and `nm ntdll.so` to `fd_cache_mutex`. ntsync misses take it around a
  wineserver call. `wine_ntsync=false` (hides `/dev/ntsync` in the sandbox)
  removed every stall. Measure before patching.

- winedbg `attach` to a running wow64 demo kills it (page fault in the break
  thread), so a single `bt all` is all one run gives.

- The tree is not rustfmt-clean, so a bare `cargo fmt` rewrites some twenty
  files nobody touched. Format only the files changed (`rustfmt <file>`), or
  skip it.

- Pointing `WINEPREFIX` at a scratch directory does not sandbox a test of
  `setup-wine.sh`: wine itself goes to `~/.local/share/demarc/wine`, and an
  unpack there without a `.version` stamp gets replaced. Set `XDG_DATA_HOME`
  too when the real one should stay untouched.

- wine's mountmgr owns any drive letter with a device link (`e:: -> /dev/mmcblk0`
  on a Deck with an SD card) and deletes that letter's `e:` as wine starts, so
  the sandbox drive must skip those too. Demos then failed at random with
  `!demarc failed`, depending on whether autodlg's CreateProcess beat mountmgr.
  `could not open working directory E:\` was in the good runs as well, which
  made it look harmless. The Deck has bwrap 0.12 now and *is* sandboxed.


- Trimming Bevy features: without `bevy_post_process` the frame renders black
  with no error in the log, even though demarc's `PostProcess` is its own code.
  Check a headless screenshot after every feature change, and put a few
  `wait_frames` after `screenshot()` -- one right before `quit()` is never
  written ("sending on a closed channel").

- `panic = "abort"` would save ~9 MB but breaks Luau: its errors are C++
  exceptions thrown through mlua's Rust frames, so any Lua error -- even inside
  `pcall` -- terminates the process.

- Not every db entry has a `date:`; `game_info.year()` is 0 then. Anything
  derived from the release year needs a fallback (the year at the end of the
  party name) or the result shows up as year 0 / 1.

- The wine prefix reports Windows XP: winetricks' `speechsdk` verb ends with
  `w_set_winver winxp`. UE4 checks `VerifyWindowsVersion(6, 0)` and silently
  picks OpenGL, so a D3D-only cook fails with "GlobalShaderCache-GLSL_150.bin
  is missing" (Dying Stars). Shipping UE4 builds log nothing; the launcher's
  `ED3 ` argument and `+process` ruled out `-opengl` before the version.

- A ddraw demo frozen on its first frame with the music playing need not be a
  presentation bug: Lasse Reinbong '96 (`zoo.198069`) polls `MCI_STATUS` from
  its render thread, and wine's MCI refuses every thread but the opener's with
  `MCIERR_INVALID_DEVICE_NAME` (0x107). `+relay` grepped for `ret=004` showed
  150k failing `mciSendCommandA` calls; three cnc-ddraw ini variants were tried
  first. Also: a db override in the built binary wins over an edit in
  `system/` until rebuilt, so check `~/.cache/demarc/system/overrides.toml`
  before believing a run was without cnc-ddraw.
