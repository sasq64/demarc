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

- A load only reaches the cross fade spare (and so the DJ window) as an
  *advance* — `run_next`/`run_prev` on a view, which `hijack_load` moves over. A
  `LoadFile` written directly loads onto the view on screen.
