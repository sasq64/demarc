# Learnings

- `--headless` has no primary window, so egui never runs: the fuzzy list and
  other dialogs can't be opened or picked from a `--remote-control` script.
  Test picker logic with unit tests on the `FuzzySource` instead.
