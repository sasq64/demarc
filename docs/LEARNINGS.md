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

- A JDK 11 demo that freezes for 0.2–1.5 s at a time in its first minute,
  then runs smooth (Transformer 3, `zoo.202844`): HotSpot's
  `NativeLookup::lookup_critical_entry` runs on the Java thread each time a
  native gets its wrapper, enumerating every module through psapi and running
  dbghelp's `recalc_search_path` after a `LoadLibrary`. Under wine that is
  thousands of `read_process_memory` server calls each time, and LWJGL has a
  native per GL function. `-XX:-CriticalJNINatives` on a line under
  `[JVMOptions]` in `app/*.cfg` fixes it; `JAVA_TOOL_OPTIONS` tries it. Not
  JIT, GC or ntsync, though C2 is busy then too. What found it: sampling
  `/proc/<tid>/syscall` showed the render thread in `read()` of 64 bytes (a
  wineserver reply) during stalls instead of the DRM syncobj wait it is in
  when smooth, then `+server` named the request. Tooling dead ends: perf
  callchains are blocked here (`perf_event_paranoid=2`), wine's unix-side
  stack holds no PE frames, the demarc wine log stops after 4096 distinct
  lines (useless with `+timestamp`), and `winedbg`'s `bt` fails on a JVM
  thread — `attach`, `break *addr`, `x /3000x $rsp` and matching the words
  against jvm.dll's `.pdata` worked.

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
  with `boot`. Check the zip for a PE first: Gracchus (`zoo.9252`) ships a
  party `.exe` beside the compo `.com`, so `boot` alone fixes it.
  A UPX-packed `.com` (AcidIce `zoo.67463`) is the same dropper once
  unpacked; `tools/unupxcom.py` ports UPX's COM stub, no `upx` needed.

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
  `ED3` argument and `+process` ruled out `-opengl` before the version.

- A ddraw demo frozen on its first frame with the music playing need not be a
  presentation bug: Lasse Reinbong '96 (`zoo.198069`) polls `MCI_STATUS` from
  its render thread, and wine's MCI refuses every thread but the opener's with
  `MCIERR_INVALID_DEVICE_NAME` (0x107). `+relay` grepped for `ret=004` showed
  150k failing `mciSendCommandA` calls; three cnc-ddraw ini variants were tried
  first. Also: a db override in the built binary wins over an edit in
  `system/` until rebuilt, so check `~/.cache/demarc/system/overrides.toml`
  before believing a run was without cnc-ddraw.

- A DOS demo that quits at once on "any key" (Fukwit Daddy) may be polling
  port 60h with no keypress yet: dosbox-pure left it at 00, a make code. Log
  `read_p60` before reasoning about frontend input; picking the exe from the
  start menu left 9C (Enter up) there, which is why that worked. Test db
  entries with `--db demodb/demozoo.txt.gz` and `load_demo(id)`: the bare
  zip lacks the db's `dos4gw` tag and fails differently.

- A running demarc can't be attached to here: `ptrace_scope=1` blocks gdb and
  `perf_event_paranoid=2` blocks perf, even on `cpu-clock:u`. Per-thread CPU
  and voluntary context switches from `/proc/<pid>/task/*/{stat,status}`
  were enough to find the stuck thread; check those first.

- Before emulating a missing vendor GL extension, check the db line for later
  downloads (`dlname` "fix", `_patch`, `_patched`): Planet Loop (`zoo.9072`)
  needs NVIDIA's EXT_vertex_weighting, while the group's own Radeon patch
  does not, and `file = "ploop_patched.zip"` was the whole fix.

- `WINEDEBUG=+relay` through demarc is cut short (demarc's wine log stops
  after a few thousand lines), so a relay of the demo's own calls came back
  empty. Run `gamescope --backend headless -- wine demo.exe` into a file for
  relay traces. The remote-control Luau has no `os.getenv`; write one script
  per run instead of passing a path in the environment.
- A `.bat` in a Windows release can be a cabinet (`MSCF`) whose stored file
  name is the batch text (Calodox 4Ks). Wine's `extrac32 cab dest` treats
  `dest` as a filter and extracts nothing, so demarc unpacks the cab itself.

- A packed DLL (te-2rb's `bass.dll`, `zoo.270858`) can still take a plain
  offset `patch`: point the unpacker's final jump into the zero tail of its
  stub section (mapped when the raw size covers it) and write the fix bytes to
  the decompressed code there. Dump the live code from `/proc/<pid>/mem` to
  find the bytes; `perf record -t <tid> --user-regs=...` gives the registers
  of a spinning loop without ptrace.

- The Lua key names follow winit: `Key.ArrowDown`, not `Key.Down`. An unknown
  name is nil, and the script dies without a word, so no screenshots appear.

- Scripted clicks in a Windows session landed at (0,0) once wine clipped the
  cursor to a fullscreen window: in that mode wine moves its cursor by XInput2
  raw motion only, and a gamescope warp makes none (`+cursor` says "X/Y axis
  valuators not found"). `+msg` shows it as `WM_LBUTTONDOWN ... lp 0`. The
  libretro backend's warp now sends relative motion first (`wlserver_mousewarp_raw`):
  it pins the cursor to the corner once and then moves it by differences.
  Pinning on every warp queued moves to (0,0) ahead of each press, and a demo
  reading the last message's position saw the click there (te-2rb).

- Mesa GL errors in a wine demo's log are not always the problem: Chimera's
  `glOrtho` with left == right and a mipmap mag filter fail on Windows too.
  What was wrong was the shape: it letterboxes to a fixed share of the height,
  made for 4:3, so at 1920x1080 it was stretched sideways. Compare against the
  Pouet screenshot (`api.pouet.net/v1/prod/?id=`) early; Demozoo's API returns 403.

- "Crashes after a few seconds" can be a GPU hang, not a wine crash: wine's
  log ends with `amdgpu: The CS has cancelled because the context is lost`,
  and `journalctl -k` shows `[gfxhub] page fault ... TCP` and a gfx ring reset.
  This Way (`zoo.59612`) draws display lists built with `glEdgeFlag` in
  `glPolygonMode(GL_LINE)`; radeonsi hangs, llvmpipe and zink do not. The
  opengl32 proxy drops edge flags on radeonsi. Trace with
  `LIBGL_ALWAYS_SOFTWARE=1` to avoid more resets, then one `+opengl` run on the
  GPU for the last calls.

- "Does not start" can be a demo waiting on its own screen: Point Blank
  (`zoo.68129`) is black for 15 s of loading, then shows a calibration page
  that waits for Space. Shoot at 10 s intervals for a minute before tracing.
  Its zip has an NVDA and an ATi exe; the NVDA one quits on missing NV
  extensions, so `boot` the ATi one. `load_demo(id)` in a remote script takes
  the first db entry with that id, which can be csdb's: test an override with a
  one-line `demozoo.txt` instead.

- `load_demo(id)` without `--db` silently does nothing (the default playlist
  has no such id); pass a one-line db. Headless counts as widescreen (its
  target is 1280x720), so a wine aspect bug seen on the 3:2 laptop does not
  reproduce until `-x widescreen=false`. `Cmd.Screenshot` saves the core's raw
  frame to the cwd, which shows what gamescope delivered before demarc scales it.

- An OpenCV message box ("Null pointer ... cvGetMat", "Incorrect size of
  input array") with music playing is an AVI that VfW cannot decode:
  `WINEDEBUG=+msvideo` shows `ICLocate Could not find a driver`. The Evolution
  of Vision (`zoo.47783`) is Indeo 3 (`iv32`). winetricks' `icodecs` is broken
  on wow64, and its `codinstl.exe` has only Indeo 4/5; `ir32_32.dll` is in
  `iv5setup.exe`, which `unshield x data1.cab` unpacks without running it. Now
  in the prefix (`system/win/prefix`). `pkill -f <path>` from a Bash call kills
  that call's own shell when the path is in its command line.

- A D3D9 demo that draws far too little (one cube for hundreds, Iterate
  `zoo.25005`), the same on DXVK and wined3d, can be its timer: `fildll` of
  QueryPerformanceCounter times 1/freq on the x87, after CreateDevice without
  `D3DCREATE_FPU_PRESERVE` has set single precision. wine's QPC is 100 ns
  ticks since boot, so with days of uptime the time moves in 1/16 s steps.
  It depends on the host's uptime, so it may not reproduce elsewhere. Patch
  the behaviour flags to add 0x2. Its setup button is "Watch!", now in
  autodlg's default `--go` list.

- Slow immediate-mode GL is wine's per-call cost, not Mesa's. Adapt's
  "Balance" (`zoo.84239`) opens at 5-7 fps: about 220k `glVertex`/`glNormal`/
  `glTexCoord` calls a frame (shadow volumes), ~900 ns each under wow64. perf
  puts Mesa at ~4%. The rest is wine: each i386 opengl32 thunk makes a unix
  call and then `set_context_attribute(-1)`, a shared SRW lock taken and
  released (with an `RtlWakeAddressSingle`) on every call. The multiarch 11.17
  build reaches ~8 fps (36 vs 26 later on). To count frames, use a registry
  `RelayInclude` of `gdi32.SwapBuffers`, give each function by name (a bare
  `opengl32` matches nothing), and run under headless gamescope with
  `+timestamp,+relay`. Delete the key afterwards.

- Remembrance (`zoo.4030`) froze with `loader_section ... wait timed out`:
  `outputs.fmod.dll`'s mingw constructor calls `waveInOpen` inside its
  `LoadLibraryA`, wine's winmm starts its devices thread and waits on it, and
  the thread blocks on the loader lock for its thread attach. Returning
  `MMSYSERR_NODRIVER` from the proxy was not enough: the plugin keeps the
  handle and loops on an error box. Releasing the loader lock around the first
  open (the proxy's `open_device`) works. winmm is loaded at process start
  here, so pre-starting wine's thread from the proxy would also have worked,
  but it would bring up mmdevapi in every demo.

- A picture cropped to the top left with a black bar on one side: compare the
  mode with the session (`wine_res`) before touching the demo's own options.
  Struct (`zoo.4473`) sets its viewport from the window, so 1024x768 in a
  1600x1200 session clips at its buffers; the bar's height gave it away, and
  the Aspect setting I changed first had nothing to do with it. Only narrow
  screens (3:2 counts) get that mode, so reproduce with `-x widescreen=false`.

- An unknown key in `system/overrides.toml` fails the whole file, not just the
  entry: every override silently stops applying. Grep the log for `** Error`.

- A demo test that counts any lit pixel as video calls a dead demo working:
  Lunaquatic (`zoo.25508`) plays its music over one solid blue frame, with no
  `!demarc visible` in the log. Tell frames apart too (`video_level()`'s hash).
  `load_demo` now takes `demozoo.1234`/`csdb.5678` and needs no `--db` if the
  default dbs load, which they do when no files are given.

- A "silent still picture" from `scripts/test_demos.lua` can be a demo's own
  launcher (Ephemera, `zoo.47716`), not a hang: look at the screenshot first.
  Ephemera's ignores Enter, so it takes override clicks. `texture2DLod` in a
  fragment shader is an error below GLSL 1.30 (`wine_glsl_version=130`), and it
  can look like a demo's crash: Extatique (`zoo.37585`) faults at NULL after
  the failed link, and leaves wine's crash dialog as its still picture.

- A `RESULT Working` from `scripts/test_demos.lua` can still be a failed demo:
  Code Is My Pron (`zoo.4688`) was white with music (Mesa refused `v.x` on a
  float, though startup frames passed the moving test), and D-Four
  (`zoo.4690`) drew `'063` for `FOUR`. Look at every screenshot, and take a
  few 10 s apart when one is flat.

- D-Four's text was off by 31 because Mesa aligns `glGenLists(n >= 32)` to a
  multiple of 32 and the intro expects 1: it calls `wglUseFontBitmapsA(hdc, 1,
  122, base)` and then `glCallLists` on raw ASCII. Characters shifted by a
  constant point at display lists; the call site in `/proc/<pid>/mem` showed
  `base` was `glGenLists`' result, after reading the relay's `first=1,
  base=0x20` as swapped arguments cost a detour. The opengl32 proxy now hands
  out low numbers.

- Epsilon (`zoo.23405`) was a frozen white frame that looked like a hang, but
  the exe had already quit: `ps` found no process, and a `+relay` run ended
  `wglGetProcAddress("glProgramParameteriEXT")` then `ExitProcess(0)`. Its
  dumped shaders were a red herring: they came from the demo tested before it.
  Check that the demo is alive before reading its log.

- A second `[zoo.N]` for an id already in `system/overrides.toml` is a TOML
  "duplicate key": the whole file stops applying, and so a working override
  (Superhero's clicks) looked like a regression for two runs. `grep` the id
  before adding an entry, and the run's log for `** Error`.

- `timeout` around a probe that overruns loses the buffered tail of the pipe
  and leaves stale screenshots behind: delete the old shots first.

- Haxholm (`zoo.21095`) needs a VfW decoder for MP42, which wine lacks: the
  prefix takes `winetricks ffdshow` and a `vidc.mp42` key. `regsvr32 /u` on
  `ffdshow.ax` breaks it, since `ff_vfw.dll` needs ffdshow's COM classes
  (`ICOpen` then fails with the driver found), so only its entries in the
  DirectShow filter list are deleted. The installer exits 1 without a display;
  run it under headless gamescope when testing.
  demarc ignores `WINEPREFIX` (always `~/.wine-demarc`): a copied prefix has to
  be tested with `wine demarc-autodlg.exe --launch` under gamescope.

- A remote-control script that fails (`error()`, or `test_demos.lua` finding
  nothing at its path) leaves demarc idling forever, and `RUST_LOG=retro=warn`
  hides the message: an empty log with no gamescope child sat for five hours.
  Scene.org awards in the db stop at 2012, so `Scene.org 2013` is empty, and
  `get_navigator` returns an empty list there, not nil. Run a sweep with a
  time limit, and list the path first.

- Candy (`zoo.42678`) faulted reading NULL right after a device vtable call at
  0x17c: slot 95 of `ID3D10Device` is `OpenSharedResource`. Counting the slot
  and `DXVK_LOG_LEVEL=debug` gave `Cannot create shared texture` with feature
  level 0x9100, since Direct2D interop made its D3D10.1 device at level 9.1.
  `tools/d3d10_1` raises that to 10.0. `d3d*=n,b` in `windows.rs` is no
  wildcard to wine, so the proxy stayed builtin under demarc until it got its
  own override, while plain wine with `d3d10_1=n,b` had already shown it working.

- A running demo can still be the wrong shape: Candy's frames looked right, but
  beside the release's `ss.png` circles were ellipses, 75% height with bars. Its
  picture height is a share of a hard-coded 1024x768 window (`CreateWindowExA` in
  `+relay`), so DXVK's "Buffer size: 1920x1080" said nothing about it. Trying
  three `wine_res` values showed the ratio stays, and 1024x768 is the true shape.
  A `+relay` of a live D3D demo writes 2.6 GB in 40 s: keep it short.

- "Plays with music and moving frames" says nothing about frame rate: Candy was
  called fixed on shots 10 s apart while it ran at 10 fps. `video_level()` every
  frame, counting hash changes per second, measures it (a `wait_frames(1)` beside
  it halves the ceiling to 30, which hid how fast the later scenes ran).
  Wine's d2d1 compiles six shaders per render target and Candy makes one per
  frame: `+d3dcompiler` showed the same sources 138 times a second. `tools/d3dcompiler_47`
  caches `D3DCompile`; `d3dcompiler_47=b` alone only gave 22 fps.
  Two wrong turns: a profile taken after the slow scene ended blamed the synth
  thread's spin on `GetCurrentPosition`, which is by design (profile inside the
  slow window, per thread), and `perf` here sees no kernel samples, so a thread
  at 84% that perf calls idle needs utime/stime from `/proc/<pid>/task/*/stat`.
  What is left is 13 s at 23 fps with `dxvk-submit` at 77% sys and the GPU at
  under 50%: time in amdgpu's submit path, not reachable from an override.

- `Could not find a plugin for file` from musix also means every plugin that
  claimed the name failed to load it, so it says nothing about the name. Modland's
  `TFMX/` songs play only with their `smpl.*` beside them, while `TFMX ST/` has 12
  songs with no `smpl.*` that fail and one with it that runs silent. Renaming
  them to UADE's `mdst` prefix did not help. Check a format's odd cases with
  `audio_level()` before listing them: `running` alone proved nothing here.

- A format in UADE's `getSecondaryFiles` table is not always one musix will
  open: `sng` is there, but commented out of `supported_ext`, so a Richard Joseph
  pair fails with `No system recognized`. Before adding a row to
  `SECONDARY_FILES`, play a whole pair; picking test songs with `head` on the
  index can also land on ones with no secondary file, which are hidden.

- Interceptor (`zoo.6279`) and Relais (`zoo.3299`) were listed as GeForce3-only,
  but on an RTX 4070 every NV function they need still exists, and only
  `glColorTableEXT` was NULL. The engine's one "Critical Warning" box hid three
  failures: that NULL, a `WGL_SWAP_COPY_ARB` no format matches (the exe's log
  is block-buffered stdout, so its reason never appears), and a `glGetError`
  of 0x500 from `GL_COLOR_INDEX8_EXT` textures. A stub for an entry point is not
  proof the feature is unused: Interceptor never calls it, Relais does, and
  in BGR, which a first emulation ignored while the frames still looked fine.
  Log the arguments of an emulated call before trusting a screenshot. The prefix's
  `syswow64/opengl32.dll` is only replaced by `--setup-wine`, so a rebuilt proxy
  has to be copied there for a test.

- `-x boot=<exe>` does not change which exe a release starts: bruh (`zoo.361793`)
  ran `msdfgen.exe` again, and a run looks like a demo test unless the `launched`
  line is checked. To try another exe without an override, pass a copy of the
  release directory with the wrong exes deleted.

- A launcher that stays on screen after autodlg's `click "Start"` need not have
  ignored it: bruh's took the click, destroyed its window and sat in `GetMessageA`
  checking a flag. `BM_CLICK` is a sent message, which never returns that call,
  and with its window gone no key or override click could wake it, so Enter
  and a click on Start both looked like a dead launcher. A `+relay` of the
  thread after `BM_CLICK` showed it; autodlg now posts `WM_NULL` after a click.
  The leftover picture was the session's stale frame.

- Plain SAD block matching cannot find a thin moving edge on a textured
  background: every static pixel in the tile votes for zero, so a solid rectangle
  over noise gets no motion vector at all. Scoring changed pixels alone fails the
  other way, since one that needs both directions to match is fooled by a
  checkerboard, and one that needs either direction cannot tell speeds apart on a
  uniform background. `crates/frameconv/src/me.wgsl` takes either direction and
  penalises landing in the changed band, checked both ways because a tile can see
  only part of that band. A numpy model of the shaders found all of this before
  any GPU code existed. Still inexact: speed 2 and up over a periodic or few-colour
  background, where the background matches itself in the uncovered band.

- A `frameconv` test helper that walks 3M pixels per frame took over 15 minutes in
  a debug build, and looked like a hung GPU; bulk copies brought it to 95 s.

- Rectangle tests said nothing about text in `frameconv`: a penalty that found the
  speed on a uniform background also punished the true vector whenever it landed on
  another stroke, so characters flickered while blocks stayed perfect. Three things
  found the causes quickly. A run with perfect oracle vectors separates estimation
  errors from interpolation ones. A regular bitmap font finds what a TrueType font
  misses: strokes on a pixel grid tie a vector with its opposite and with its own
  multiples (speed equal to the font's pixel scale was the worst case). And frame
  sides have to be left out of a text measure, since entering text is in only one
  of the two frames, which looks like an interpolation bug.

- Judder under `--frame-convert` is more often pacing than motion estimation:
  run with `RUST_LOG=demarc::emulator=debug` and look for `frameconv: dropped` /
  `repeated` lines at the jumps before touching `crates/frameconv`. A core
  that isn't exactly 50 Hz (C64 PAL is 50.1245) drains the audio ring faster than
  the proportional `rate_adjust` can hold above `audio_buf_min`.

- RIFE (rife-v4.6) takes 3 ms per 384x272 frame on an RTX 4070 in a tight loop
  but 10.5 ms called once per display frame from demarc's render thread: its own
  VkDevice shares the GPU with wgpu's. Measure inside the app before deciding
  where an ncnn call can run; it has to run ahead on a worker thread
  (`rife::Worker`), and `Emulator::conversion_ahead` shows it which pairs come next.
- `rustfmt src/main.rs` formats every module it declares, and the repo is not
  rustfmt-clean at the default width, so it rewrites unrelated files. Format the
  files you changed only through a crate whose root you did not touch, or not at all.
- `Emulator::display_fps` is smoothed, so at startup it climbs through every
  rate below the real one. A conversion that latched as soon as the display was
  faster than the core picked 50:51 on a 60 Hz screen: phases in 51sths, nearly
  every display frame needing RIFE, and audio drift dropping source frames. The
  latch now needs one whole rate for `CONVERT_LATCH` frames in a row.

- Exiting the app early (an `AppExit::error()` in the first seconds) with
  `std::process::exit` segfaults: librashader is still building shader chains
  on rayon threads, and the atexit handlers free glslang's statics under them.
  `main` uses `libc::_exit` for the error exit.

- Don't run `cargo fmt`: the tree is not rustfmt-clean (lines run past 100),
  so it rewrites dozens of unrelated files.

- Judge ffmpeg decode cost paced (`-readrate 1`) on a high-bitrate clip, not
  flat-out on `testsrc2`: flat-out runs inflate thread overhead, and testsrc2 is
  so cheap to decode that `-hwaccel` looked like a loss (the VRAM download costs
  more than the decode), while on a ~250 Mbit/s clip it cut CPU from ~3 cores
  to ~1. Mesa's VA-API `scale_vaapi` ignores colour tags (untagged BT.601 came
  out as BT.709, a tagged clip came out limited range), so keep the RGBA
  conversion in ffmpeg's software scaler.

- A backend fed by separate decoder threads must not `try_recv` its streams
  per frame: each miss drops that stream a frame for good, and the ffmpeg
  backend drifted ~1 s out of sync. Block on both in `run()` so they advance in
  lockstep; counting frames vs samples over 300 `run()`s shows the drift.

- A/V sync while frame converting can't be checked headless: there is no audio
  device, so the ring fill is a constant 6000 and the queue never moves. A
  Python model of `Conversion::step` with a draining ring showed the queue
  creeping from ~5 to `depth` (18) over a minute, putting the picture ~0.55 s
  behind the sound.

- Sync while converting depends only on the queue length and the ring fill as
  long as each queued frame came with its own sound, so gaps (dropped frames,
  a worker's free-run) heal once they leave the queue. Before blaming one,
  record `--frame-log` and read the `av_offset` column.

- The phases in `RIFE frame at .. not ready` lines give away the cadence: they
  step by source/display rate (25/144 = 0.1736), which tells which file and
  display a pasted log came from. A frame log with the ring pinned at
  `audio_buf_max` and `av_offset` falling meant the cadence was outrunning the
  sound and nothing held the picture back until the queue was empty.

- Cycles picked in the settings UI only go into the system's meta, which is
  read on the next load, so changing them while a demo plays does nothing; and
  the highest fixed step equals the 200000 default. Test with `-x
  dosbox_pure_cycles=<n>` and count `video_level()` hash changes per frame.
  Trem muls (`zoo.23638`) went from 110 to 600 changes in 600 frames at `max`.
  Its flood of `DMA segbound wrapping` lines (logged per byte) looked like the
  cause but was not: logging once per block changed nothing.

- The Seeker (`zoo.96729`) blew out to white through its glow. The cause was
  a render target that deletes its old FBO and renderbuffers before making
  new ones, from fields never set: wine's heap left a live FBO id (the DOF
  downsample's) there, so `+opengl` showed `del_object Deleting framebuffer
  client 0xa` and the next `glGenFramebuffers` handed that id to the scene.
  An IFEO `GlobalFlag` of 0x20 (heap free checking, which fills new blocks
  with 0x55) in `setup-wine.sh` fixes it without touching the UPX exe;
  `WINEDEBUG=warn+heap` does the same for a test. Running a downloaded `upx`
  is refused by the permission classifier.
- Bisecting post-processing with `glsl` patches: one shader source can serve
  several passes (The Seeker's blur is both DOF and glow), so zeroing it
  changes more than the pass in mind. Match on a uniform only one pass has,
  or trace which program each FBO pass binds first.
- demarc's wine log drops repeated lines and stops at 4096 distinct ones, so a
  per-frame `+opengl` sequence needs plain wine under `gamescope --backend
  headless` with stderr to a file. Pointer-argument calls (`glFogfv`,
  `glColor4iv`) show no values; a temporary logging hook in the opengl32 proxy
  does.

- ffmpeg's `color=black` source does not decode to RGB exactly 0 (YUV range
  conversion leaves small values); test for "lit" pixels with a threshold.
  In filtergraph arguments `:` is escaped only at the option level (`\:` →
  `\\:` after graph escaping), not twice.

- MicroW8 (`uw8` core): an invalid cart makes the core print a wasm3 error and
  call `exit(1)` inside `retro_load_game`, killing demarc with no log line.
  `examples/rust/tunnel.uw8` in the microw8 repo is itself corrupt (UTF-8
  replacement bytes); test with `site/static/uw8/skipahead.uw8`.

- In TOML, every key after a `[table]` header belongs to that table, so a theme
  file's top-level keys written below `[palette]` silently became palette
  entries. The theme has no top-level keys for that reason; keep it that way.

- A demo crashing at random places inside ntdll's heap (`main process heap
  section ... wait timed out`, `err:heap` from several threads under `+heap`)
  can be the exe calling `HeapAlloc(GetProcessHeap(), HEAP_NO_SERIALIZE, ..)`:
  harmless alone on 2004 Windows, a race once DXVK's threads share the heap.
  Check the flags with `+relay` before hunting an overrun (.kkrieger beta).

- Under heavy `WINEDEBUG` (`+relay`, `+virtual`) demarc's forwarded wine log
  stops after a few thousand lines and the timing change can hide the bug, so
  two runs showed nothing. Reproduce with plain wine under `gamescope
  --backend headless -w 1920 -h 1080` (the session size matters for heap
  layout) and send stderr to a file; `winedbg --file` there gives the stack.

- Overrides are keyed on the db entry: a demo launched from its zip path gets
  none of them. Test an override with `load_demo("demozoo.<id>")` in the
  script and no file argument.

- Tannhäuser Gate (`zoo.80364`) worked on AMD but died on NVIDIA with a GLX
  `BadMatch` in `X_GLXMakeContextCurrent`. The `aux=0` patch only drops the
  aux buffer from the format; RenderTexture still calls
  `wglBindTexImageARB(WGL_AUX0_ARB)`, which the opengl32 proxy passed on to
  wine's internal-context copy. demarc's `+opengl` log was cut off before the
  error; plain wine under headless gamescope with autodlg's `--launch` and
  stderr to a file showed the last call. The prefix copy of a rebuilt proxy is
  not refreshed by a demarc build: install it into `syswow64` (or rerun
  `--setup-wine`).

- Ballad of a Cluster Bomb (`zoo.878`) showed only its canvas. I spent a long
  time on a wrong lead because a `sed` edit to an `.fx` file did not match
  (tabs vs spaces) and the "solid colour" test silently ran the original
  shader. Grep for the edited line before trusting a shader experiment. Making
  each post-process pass return a solid colour, one at a time, found the pass
  that drew nothing in a few runs; reading wined3d traces did not. The cause
  was `Lock(0, 4, ...)` (vertex count passed as bytes) followed by a 96-byte
  copy. The site was missed by grepping for `push 0x4` next to the vtable
  call, because the size came from a stack argument: search for the `+0x2c`
  vtable load and read the pushes around it. DB entries with several downloads
  only fetch the first; the user asked whether v1.03 had been tested, and it
  had not (it behaved the same).

- B - Incubation (`zoo.172`, asked for as `zoo.782`, which is a mod): check
  the id against `demodb/demozoo.txt` and the newest download in
  `~/.cache/demarc/downloads/`. `wine_glsl_version=130` fixed every compile
  error, but the intro then exited silently at its loading screen; a frame
  that did not change in two minutes was the loader, not a hang, and `ps`
  showed the exe gone. A `glsl` patch that adds `#version 130` only to
  shaders containing `%` (and `#version 120` to ones using `transpose`) runs
  it to the end. Its dialog's start button is "Test", now in autodlg's
  default `--go` list.

- fr-065 Euphotic (`zoo.5151`) crashes at random places in werkkzeug3's mesh
  code, so a single run proves nothing either way: patching one of its four
  Georgia text ops "fixed" it in a few runs and crashed in the next demarc
  run. Under winedbg (debug heap fill 0xbaadf00d) it fails every time, which
  made it testable: all four credits in Verdana load, any one left in Georgia
  crashes. Zeroing every heap allocation through a test kernelbase did not
  help, so it is not plain heap garbage. Parallel test runs change the timing
  and the crash rate; compare runs made the same way.
- A demarc `--remote-control` run killed by `timeout` leaves its gamescope,
  bwrap and wine processes behind; check `ps` for a stale `gamescopereaper`
  after any timed-out run.
- Glowsick (`zoo.1526`) "black" was a stale frame of its MFC setup dialog: the
  dialog is MFC's main window, its OnNcDestroy posts WM_QUIT, and the demo's
  PeekMessage loop exits on it right after loading. `+relay` showed one
  PeekMessage and then ExitProcess; NOP the AfxPostQuitMessage call.
- Phon (`zoo.1869`) quits silently after a GLEW check that wants both
  `GL_ARB_geometry_shader4` and `GL_EXT_geometry_shader4`. NVIDIA lists only
  the ARB one, and wine refuses the EXT functions; the opengl32 proxy now adds
  the EXT name and maps its functions to the ARB ones. Mesa has neither, so it
  stays NVIDIA-only. `objdump` cannot read a UPX exe and Yama blocks
  `/proc/<pid>/mem` even from an ancestor of bwrap's pid namespace; `upx -d`
  (installed now) or winedbg are the way in. A winedbg breakpoint on the
  unpacked exe on ripper read the GLEW flags.
- The test script's "silent and black" for a 4k executable graphics entry may
  be a still picture: Burj Babil (`zoo.919`) was black only because its shader
  needs `wine_glsl_version=130` (`switch`); a lit still picture in that
  category now counts as working.
- A demo that never pumps messages (polystreptikum, `zoo.66322`: its loop is
  only waveOutGetPosition/GetAsyncKeyState/SwitchToThread) hung autodlg's
  `fill_desktop`: SetWindowLongPtr/SetWindowPos on another process's window
  send to its thread and wait. "the dialog driver never said the demo was up"
  then hid the first ten seconds. autodlg now leaves a frameless window at the
  origin alone; wined3d's WS_SYSMENU on its fullscreen window is not a frame.
- A "No Start" entry for another platform can be a release that bundles a
  Windows emulator (Mescaline Synesthesia, `zoo.70380`, ships Unreal Speccy);
  demarc started the exe. `--boot-file <image>` tests the native image before
  writing a `boot` override.

- The "Nvidia Problem" playlist was two causes, not one. Led Blur
  (`zoo.144558`, SDL 1.2) and Pornonoise (`zoo.3302`) are wined3d ddraw:
  front-buffer drawing reaches the session on AMD but stays black on NVIDIA,
  so a ddraw demo that works on the framework machine can still need
  `wine_cnc_ddraw`. Die Ewigkeit Schmerzt (`zoo.9245`) was not a GPU problem:
  the release-dir `--tmp-overlay` added for `tmpfile()` threw away the
  precalc cache that `cache_writeback` exists to keep, so every run sat ~100 s
  on a white frame. The framework machine had its cache from before that
  change. In-place releases now skip the overlay.

- Lua under `--remote-control` has no `os.getenv`: a script that reads one
  fails silently and the run sits until the timeout with an empty log. Write
  one script per demo instead.

- `test_demos.log` holds only the tracing log; the script's `print` lines
  (the per-demo reasons) go to stdout, so a sweep has to be rerun to see why
  a demo landed on "Test: No Start". Every entry there is silent by
  construction (sound sends a demo to Working or No Graphics), so an
  all-silent triage is not a broken capture.
- A web release's page was the first `.html` in walk order, which in a
  release shipping `node_modules` was `LICENSE-W3C.html` (Holly, `zoo.104135`).
  The web system now takes the shallowest page, `index.html` first.
- Two Crinkler intros (Cassini `zoo.367582`, Eisenerz `zoo.266556`) die on a
  call into an unimplemented wine user32 stub (`SetUserObjectCapability`,
  `ShellHandwritingHandleDelegatedInput`), reported "from" kernelbase's
  `RaiseException`, not the intro. Both stub names sort just before
  `ShowCursor`, which both intros import: a hash collision with names wine
  exports. wine's stubs call `___wine_spec_unimplemented_stub`, so a proxy
  could leave them out.
- Horizon Machine (`zoo.170744`) "black and silent" was two faults. The exe
  was gone after its 4klang precalc: `ps` showed it at 96% CPU, then nothing.
  A relay under plain gamescope showed the last call, `wglUseFontBitmapsA`,
  faulting on a 333 MB glyph: its packed LOGFONT is 0x8000000 wide (`+font`
  shows the `NtGdiHfontCreate` fields). The opengl32 proxy now gives such a
  font width 0. Then it was white with music: `#version 430` with
  `gl_FragColor`, which NVIDIA rejects and Mesa takes, so the software-Mesa
  dump compiled cleanly and only `glslangValidator` on it showed the error.
- From The Seas To The Stars (`zoo.192022`) is black for 60-90 s of Oidos
  precalc and then plays; the sweep's 30 s window called it dead. Dreamtime
  Return (`zoo.307507`) ships `_1080p`/`_1440p`/`_2160p` exes and the 2160p one
  was started; `res_from_name` now reads `<height>p`. Bull (`zoo.292424`,
  Leviathan 2.0) draws its scene twice and then copies the back buffer each
  frame, expecting it to survive the swap; under wine on NVIDIA it does not.
  Its swaps go through gdi32 `SwapBuffers`, so an opengl32 proxy swap hook
  never runs: check `+relay` for `gdi32.SwapBuffers` before writing one.
- ReelTime (`zoo.379064`) drew boxes for its thumbs up/down: wine's builtin
  `wingding.ttf` has only 49 glyphs, and `+font` showed Wingdings found, not
  replaced, so the font list said nothing was wrong. Check the font's `cmap`
  for the drawn codes (`F000 + byte`) with fontTools. `tools/wingdings` builds
  a full one from Noto, installed as `Fonts/wingding.ttf`.

- A web demo whose loader stops on one effect: read the console over CDP
  (`chromium --headless=new --remote-debugging-port`, a node script with the
  built-in `WebSocket`; plain `--enable-logging` printed nothing). Headless
  Chrome falls back to SwiftShader, which hides GPU-only failures: pass
  `--ozone-platform=headless --use-angle=gl-egl` to get the real driver.
  WUNDERLUST (`zoo.376145`) threw "Cannot bind uniform ... is not used" on
  ANGLE-GL, where NVIDIA drops a sampler the shader never reads; ANGLE-Vulkan
  keeps it, as D3D does on Windows. `gamescope_chrome_args` passes the flag.

- Brainworm (`zoo.202448`) faulted executing address 0 right after a heap
  alloc. A `+relay` pairing of `GetProcAddress` calls with their return values
  found it at once: `msvcrt._libm_sse2_sqrt_precise` returns NULL. Windows 10's
  msvcrt exports it, wine's has it only in ucrtbase. kernel32's
  `GetProcAddress` is its own code (not an import of kernelbase) and ntdll's
  `LdrGetProcedureAddress` has no hot-patch prologue, so the kernelbase proxy
  overwrites kernel32's entry with a jump to a wrapper instead.
- Variform 3 (`zoo.166539`) is UE4's stock `UE4Game-Win64-Shipping.exe`; its
  empty "Fatal error!" box is the winxp prefix again (as with Dying Stars). A
  shipping build writes no log, so try win10 before reading traces.
- Dragon Rising (`zoo.145076`): an MSVC "invalid null pointer" assertion was
  just the missing command-line argument from `run.bat`. Read a release's
  `.bat` before debugging the exe. It then compiles shaders for ~30 s, so a
  short test window calls it black and silent.
- Le Laboratoire D (`zoo.181204`) crashed only in demarc. Plain wine, autodlg
  `--launch`, and a hand-built bwrap with the prefix overlay and pid namespace
  all worked; adding the sandbox's `--symlink <release> dosdevices/<x>:` made it
  crash, and the same symlink to an empty directory did not. Bisect the bwrap
  argument list by hand when `wine_sandbox=false` fixes a demo.
- winedbg tips from Tension and Observatory: a `break` on a builtin's symbol
  ("No symbols found") fails, but a break on its address works
  (`0x7be60000` + export RVA for i386 kernel32). Hit an import the exe calls
  after unpacking (`SizeofResource`), then set the breakpoints in the unpacked
  code. `x/s *(int*)($ebp+8)` dereferences once. `attach <pid>` from a
  winedbg command file into a running wow64 process faults; launch under winedbg and
  drive the dialog from a separate `demarc-autodlg.exe` without `--launch`.
- Observatory (`zoo.109195`) showed its "File <" box in plain wine too, once
  the dialog was answered; my first plain run had just sat at the dialog and
  looked fine. Launch through autodlg when comparing plain wine with demarc.

- A link that used to download an HTML page (e.g. a Google Drive share link
  before it was rewritten) stays cached as that page under
  `~/.cache/demarc/downloads/<hash>/`. When testing a download fix, delete the
  old entry first or the old result is just reused.

- Web releases now load over a local HTTP server, not `file://`. A PNG-packed
  page (`<img src=#>` decoding itself) still "showed raw text" over HTTP; the
  text is the PNG rendered as HTML, and stays visible when the script fails
  behind a transparent canvas. Read the console over CDP before blaming the
  transport: Newton Protocol (`zoo.202470`) threw on a renamed WebGL method,
  and the party zip of Decompress of Empty had a PNG with its CR bytes lost. A `--remote-debugging-port`
  passed through `gamescope_chrome_args` was not reachable from outside the
  session; reproduce in plain headless Chromium against `python3 -m http.server`.
- Edits to `system/overrides.toml` only reach a release build after
  rebuilding (it is read from the embedded `system.zip`). Three Decompress of
  Empty runs tested an old override and blamed the click; the "Found override"
  debug line shows what was actually loaded.
- scene.org's `de-https` mirror (NetCologne) puts a welcome page in front of
  every `.html` it serves, so a web release read as text. The fetcher now
  treats that as a failed mirror.
- `cargo fmt` reformats the whole workspace (it is not fmt-clean); format only
  the files you changed (`rustfmt <file>`).

- A kkrunchy exe can be patched without unpacking it (Tension, `zoo.269513`).
  The stub ends with `emms; mov esi,<imports>` before its import loader; a
  jump from there into the zero padding after the import directory runs a
  cave that retargets the untransform's final `je <OEP>` (decompressed by
  then) at a second cave, which patches the unpacked code and jumps to the
  OEP. Under winedbg, breakpoints on not-yet-unpacked code are lost; break
  in the stub or the cave.
- Tension went silent at random. I spent a long time on "NaN" in its audio
  buffer, read as float; the source voice's WAVEFORMATEX said 16-bit PCM and
  the music was fine. Read the format before the samples. The real fault was
  a `vkAllocateMemory Exception 0xc0000005 in Unix call` in the log just
  before exit: a size loop that never reset asked for 32768-wide RGBA16F
  textures, which the D3D11 runtime refuses above 16384 and DXVK passes to
  the driver. `audio_level` zero with a moving loading screen can be a demo
  that already exited, leaving its last frame up.
- Observatory (`zoo.109195`): native msvcr100/msvcp100 did not help (and
  wine loads its builtins even with native ones beside the exe until
  `msvcr100=n` is set). The use-after-free is a setter called with its own
  member's `c_str()`: it reassigns the member, then reuses the argument.
  Look for the stale pointer being reread after the `operator delete` in the
  same function before blaming the CRT.
- Esocentrica (`zoo.167713`) loads shaders from disk and compiles them
  cleanly, yet stays black: its exe imports DDRAW as well as OPENGL32
  (Satori's Circulation engine presents through DirectDraw). Check a GL
  demo's import list for DDRAW before tracing GL; `wine_cnc_ddraw` fixed it.
  An `+opengl` trace through demarc is useless for long runs: the wine log
  keeps 4096 distinct lines, and pixel-format enumeration fills that first.
- A web demo's "Click" text at the top left is usually an `<a>` with the
  `onclick`, not the canvas ([Type] `zoo.153542`, Condition `zoo.292275`). Its
  box is only ~18 px high at the 8 px body margin, so y=0.025 at 1080 already
  misses it. Condition also shows the link only after a 20-40 s precalc;
  clicks before it land on "Wait a moment..." and do nothing, so repeat the
  click.
- Holly (`zoo.104135`) was black because its first `AudioBufferSourceNode`
  call was `noteOn`, which Chrome dropped years ago; the exception stopped the
  start. Read the console before guessing at Leap Motion or clicks. When
  re-testing a patched file in headless Chromium, use a fresh
  `--user-data-dir`: the HTTP cache served the unpatched script.
- Genesis (`zoo.267672`) had `boot = "client/index.html"`, which never
  matched: `find_named` compared file names only, so the systems' own pick,
  the node server exe, ran under wine. A name with a `/` is now a path from
  the release directory (also for patch targets, needed for its two
  `app.js`). Check the log's "launched" line before debugging the page.
  Its music was loaded from `../../music/`, the demo's own server root; under
  demarc's `/<n>/` prefix that leaves the release, and the timeline waits for
  audio, so the page was black and silent. Grep a web demo for `../` and
  root-relative URLs.
- `rustfmt <file>` also formats every module the file declares (`mod x;`),
  so formatting `newsys.rs` rewrote four untouched files. Check `git diff
  --stat` after it, or pass `--config skip_children=true` (nightly) / revert.
- Driftwood (`zoo.202824`) asserted at `common.c 1828`, which I had read as
  1028 and taken for a CPU/FPU or memory problem; `fast = true`, no FPU
  (Line-F trap, it needs one) and more chip RAM were all wasted runs. The
  assert was an `fopen` of a chunk file: the amigascne lha is missing seven
  assets the party zip has. When an Amiga demo dies loading, diff the asset
  names in the exe against the archive before touching the machine config.
  The exe's strings are inline in its code hunk; `pea #line; pea path(pc)`
  finds an assert's site, and capstone (M68K) disassembles hunk 0.
- Art Of Profanity (`zoo.306979`) ships floppies and a hard drive build; the
  floppies (preferred by the ST system) stop at the TOS logo. With `boot`
  naming the `.prg`, the picture came up but silent: the override narrows the
  work file to the program, and `build_gemdos_drive` treats a lone file as
  the whole release, so `zik.raw` beside it never reached the drive. It now
  takes the release from `RELEASE_DIR`, as the Amiga system does. A system
  that copies "the release" should check `RELEASE_DIR` when the path is a
  file.
- `pkill -f <name>` from a Bash call kills the call's own shell when the name
  is in its command line (exit 144). Kill by `ps -eo pid,comm` instead, as the
  skill already says for `pgrep -f`.
- Return (`zoo.311793`) is not a missing runtime: wine-mono asserts in
  `prepare_run_main` because `Program` has a static field typed from an
  assembly that only the app's AssemblyResolve handler (embedded `FLPK`
  resources) can supply, and mono resolves field types before `Main` runs.
- Crinkler exes are now unpacked before running (docs/CRINKLER.md). The
  depacker runs in `crates/newsys/src/x86.rs`; checking its output against a
  winedbg dump at the depacker's final `ret` (`tools`-style script: break on
  the header's `ret`, read `/proc/<pid>/mem` as a child subreaper, since wine's
  launcher exits and orphans the exe) found it exact for 1.1, 2.2 and 2.3.
  Gotchas on the way: the tiny-import DLL list has no terminator (it ends where
  `LoadLibrary` fails, Eisenerz's at "vers/gm.dls"); today's Crinkler export
  list has Win11 names that make 2.x slots ambiguous, so each version's own
  list is used; 1.x headers overlap the hash list, so some hashes are dummies;
  the tiny-import prologue pattern also matches the tail of the ordinary one.
  Don't rely on `dd`'s skip into a dump that starts at 0x400000 as if it were
  addresses (I read zeros at "0x420000" that way).
- Two headless demarc sessions at once spoil each other: a sweep running while
  I tested Organix in parallel reported every later demo silent, mostly
  frameless, and four as `retro_load_game(...) failed`. Run one session at a
  time, and treat a sweep that turns bad all at once as invalid.
- The Crinkler regression sweep (`Test: Working`'s 37 Crinkler demos, packed
  vs unpacked) found Horizon Machine calling
  `gdi32.D3DKMTAddSurfaceToSwapChain` once unpacked: the 2.0a export list has
  that name in `CreateThread`'s tiny-import slot, the 2.0 list doesn't, and a
  header says `20` for both. Most of the sweep's "silent, 0-1 frames" were the
  same packed: executable graphics show one still, and Oidos intros precalc
  past a 20 s window. Always sweep the baseline too before calling a
  difference a regression.
- The Crinkler unpacker was checked against 1.1, 2.x and 2.3 test exes and
  called done for "1.x to 3.x", but every 1.2/1.3 exe (a third of those in the
  download cache) fell back to packed: their import prologue reads the PEB as
  `fs:[bx+30h]`. Sweep a decoder over every matching file in
  `~/.cache/demarc/downloads` (an `#[ignore]` test over a directory is enough)
  before claiming version coverage, and before removing the proxies it
  replaces.
- Fabula (`zoo.377071`) hung after its first present. The validation layer
  (Arch's `vulkan-validation-layers` package unpacked to a scratch dir,
  `VK_LAYER_PATH` + `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`) named it
  at once: the present waited on a semaphore with "no way to be signaled". A
  `+vulkan` trace and winedbg had only shown the symptom. Run the validation
  layer first for a Vulkan hang. Once the hang was fixed, a GPU hang followed
  (`journalctl -k | grep Xid`, 109): the same run's
  `VkDescriptorBufferInfo-offset` errors showed the uniform ring overflowing.
- Return (`zoo.311793`) runs on native .NET 4.8 (`winetricks dotnet48`). That
  verb also moves the prefix from winxp to win7 and removes wine-mono. Test it
  in a `cp -a` of the prefix under `gamescope --backend headless` first.
- Driftwood (`zoo.202824`) was marked fixed on "sound + moving frames", but
  it froze on its first picture while the music (interrupt driven) played;
  the frame count stopped at 12 after 10 s. `amiberry_jit = "disabled"`
  fixed it. For a demo with sound, check that the unique-frame count keeps
  rising and that screenshots show different scenes before calling it fixed.
  Also: `system/overrides.toml` is copied to `~/.cache/demarc/system` by the
  build, so an override edit needs a rebuild (the log's `JIT=` line shows
  whether it took).
- Indie-Vibes (`zoo.143895`) had been filed as "not fixable": BoxedApp-packed
  Unity, faulting in BoxedApp's own loader. Look at the packer's sections before
  debugging its loader: `.bxpck` held all 37 files uncompressed, and the
  original PE (entry point, `.rdata` import table) was untouched apart from
  the import/TLS directories. Unpacking it (`boxedapp.rs`) took far less time
  than debugging the fault would have. Also, the release is a WinRAR SFX that
  demarc ran as is, so an unpack step on the release never saw the inner exe
  (`sfx = true` override). Never run `cargo fmt` here: stock rustfmt reflows
  ~45 untouched files (the tree is formatted wider); format only the files you
  edit (`rustfmt --edition 2024 <file>`) and check `git diff` for stray hunks.
- Klos's 4k gfx (Quattro `zoo.371225`, the Fast & Fouriers series) flashed
  one frame, then went black or saturated. `wine_gl_keep_back` was a wrong
  first guess. The demarc `+opengl` log hid the cause: it drops repeated lines
  and shows the proxy's emulation calls mixed in with the intro's own. A plain
  wine trace showed `glAccum`, and the intro binds texture 1 without
  `glGenTextures`, which was the name the proxy's emulated accumulation buffer
  had got. Proxy objects now take high names. The intros take 10-20 s to
  accumulate under demarc, so wait at least 1200 frames before calling one black.

- The crinkler tests run the x86 interpreter over real depackers and take ~90s
  in release but well over 10 minutes in a debug build, which looks like a
  hang. Run `cargo test --release -p newsys` when touching `x86.rs`.

- "Stops playing new demos" with the window alive and the gamescope core
  SIGSTOPped by its own pauser: cpal's ALSA thread was at 100% CPU in a loop
  that only calls the error callback, so the ring stayed full and
  `Emulator::run` dropped every frame. ptrace and perf are closed here, but
  `top -H` and `/proc/<pid>/task/<tid>/io` (millions of 20-byte writes =
  `eprintln!("audio stream error: ")` to a stderr on /dev/null) found it.
  Check the cpal thread's CPU first when emulation stops without a log line.

- Crinkler-unpacked exes that crash right after the import code but run with
  `-x crinkler_unpack=false`: check which registers the code after the import
  loop still reads. Final Stage (`zoo.170740`) `stosd`s into `edi` left at the
  IAT end; 1.0 exes (Arancia `zoo.59650`, sprite-o-mat `zoo.4286`) run their
  call transform from `edx`. The rebuilt stub set neither. Compare packed and
  unpacked first — it isolates the unpacker in one run.
