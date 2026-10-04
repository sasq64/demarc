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
