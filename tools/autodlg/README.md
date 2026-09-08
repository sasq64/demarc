# demarc-autodlg

Answers the setup dialog nearly every PC demo opens with, so nobody has to sit
there and press Start. It is a small Windows exe that runs inside the same wine
prefix and session as the demo, walks the Win32 control tree (`EnumWindows` /
`EnumChildWindows`) and drives the controls by message rather than by clicking
pixels — so the resolution can be *chosen* rather than accepted, which is what
demarc needs for a fixed capture size.

It also starts the demo and outlives it, which is the only way anything outside
the session can learn that the demo has ended.

Build with `just autodlg` from the repo root; that also copies the exe to
`system/win/demarc-autodlg.exe`, where `src/wine.rs` looks for it.

## Usage

```sh
wine demarc-autodlg.exe --launch demo.exe --prefer 800x600,640x480 --check Fullscreen
```

Arguments are plain `--name value` pairs (no `=`, no short forms). Flags take no
value. Unknown arguments print a warning and are skipped.

| Argument | What it does |
|---|---|
| `--launch <exe>` | Start `<exe>` from its own directory, then drive its dialog. The demo is started as a *child* so it inherits this driver's session (and virtual desktop, if there is one) — a sibling started separately would be invisible to `EnumWindows`. Also restricts every window search to that process, so an unrelated dialog can never be answered by mistake. Without it, the driver only inspects whatever windows already exist. |
| `--prefer <a,b,c>` | Select the option whose label contains `a`; if the dialog offers no such option, try `b`, then `c`. Works on radio buttons, checkboxes, combo boxes and list boxes, because dialogs offer resolution as all four. Typically the wanted mode and what to settle for, e.g. `--prefer 800x600,640x480`. Repeatable, and each occurrence is a chain of its own, so several unrelated options can be picked in one run. |
| `--check <label>` | Tick the checkbox whose label contains `<label>`, if it isn't ticked — repeatable. demarc passes `--check Fullscreen`. |
| `--uncheck <label>` | The same in reverse: untick it if it is ticked — repeatable. |
| `--go <a,b,c>` | Comma-separated labels that count as the start button. Default: `RUN,OK,START,GO,LAUNCH,PLAY,YES,DEMO`. Replaces the default list rather than adding to it. |
| `--no-fallback` | When no button matches `--go`, do nothing. By default the driver posts Return to the dialog to press its default button. |
| `--no-go` | Touch no dialog at all — just launch the demo and watch it. What `wine_res=pick` uses, so a person can answer the dialog themselves. |
| `--no-fill` | Leave the demo's window as it is. By default, once the demo's render window appears, its frame (title bar, borders) is stripped and its client area moved to the desktop origin, so the captured frame is the demo and nothing else. The client area keeps the size the demo picked; it is never resized. |
| `--list` | Print the dialog's control tree — class, kind, text, checked state, combo/list items and selection — and change nothing. This is how you work out what to pass to `--prefer` for a demo that needs a per-release override. |
| `--timeout <secs>` | How long to wait for a dialog to appear, and afterwards how long to wait for the render window to undecorate. Default `20`. Generous on purpose: a cold wine prefix spends a while building itself before showing its first window. |

## Label matching

Every comparison strips the label down to its uppercase alphanumerics, so `&Run`,
`GO!` and `-= START =-` all match plainly. `--prefer`, `--check` and `--uncheck`
match on *substring* of that; `--go` matches the whole thing, so `OK` cannot hit
a `NOT OK`. An alternative in a `--prefer` chain counts as offered as soon as one
control carries it — even an already-selected radio button with nothing to click —
and that is what stops the rest of the chain from being tried.

## Output

Everything on stdout is log text for whoever is reading it, except lines
beginning `!demarc `, which demarc parses:

- `!demarc started` — the demo process was created
- `!demarc exited` — it has ended (this is the one demarc is waiting for)
- `!demarc failed` — `--launch` could not start the executable

Exit status is 1 if the launch failed, or if there was nothing to launch and no
dialog was found; 0 otherwise.
