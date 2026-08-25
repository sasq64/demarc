+++
title = "demarc"
template = "demarc.html"
+++



## Intro

_Main goal_: Make it easy to watch oldschool (PAL) demos

Supported systems:

C64, Amiga, Atari ST, Amstrad CPC, ZX Spectrum, Megadrive, SNES, Atari 2600, Atari XL, Tic-80, Playstation, Gameboy Color, Gameboy Advance, PC (DOS, and Windows through wine)

* Runs multiple demos in order or shuffled
* Shows demo meta data as overlay
* CRT shaders (Lottes) for "authentic" look
* Can load slangp retroarch shaders
* Fuzzy search files on disk or from database file
* Displays Amiga IFF and Atari DEGAS images
* Plays music
* Can load disk images and executables
* Right-Alt/Ctrl hotkey for disk switch etc
* Can run multiple files at once in a grid
* Linux: Pause screen blanker and handle media keys

## Install (Linux, Mac)

`curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sasq64/demarc/releases/download/v1.4.0/demarc-installer.sh | sh`

## Install (Windows)

_IMPORTANT:_ Demarc downloads and links DLLs at runtime, which often makes Windows flag it as malware and silently delete it. Add an exception to your settings, or switch to a sane operating system.

`powershell -ExecutionPolicy Bypass -c "irm https://github.com/sasq64/demarc/releases/download/v1.4.0/demarc-installer.ps1 | iex"`

(the above is usually blocked by Windows. You can try downloading the ps1 script manually and executing it).

Or download the release zip: [demarc-x86_64-pc-windows-msvc.zip](https://github.com/sasq64/demarc/releases/download/v1.4.0/demarc-x86_64-pc-windows-msvc.zip)
## Rust source install

If you don't already have it, install [rust](https://rustup.rs).

Then:

`cargo install --git https://github.com/sasq64/demarc.git`

## Prepare

Set your monitor to 50Hz if possible.

## Run

`demarc --help`

`demarc <some_demo>`

`demarc --aga --shuffle Amiga/`

`demarc --db bitworld.txt --select`

_TIP:_ Download all intros from [https://intros.c64.org](https://intros.c64.org/) and run

`demarc --grid=4x3 --shuffle intros_c64_org_12596_full`

## Demo Packs

#### Databases

* [bitworld.txt.gz](/dl/bitworld.txt.gz) (1.8MB)
* [csdb.txt.gz](/dl/csdb.txt.gz) (8MB)
* [demozoo.txt.gz](/dl/demozoo.txt.gz) (UPDATED 26081011) (7MB)

#### Best of Amiga OCS (and some AGA)

* [Amiga.7z](/dl/Amiga.7z) (55MB)

#### Best of Atari ST/STE

* [Atari.7z](/dl/Atari.7z) (9MB)

#### Best of other (Amstrad, Spectrum, Consoles etc)

* [Other.7z](/dl/Other.7z) (16MB)

#### CSDb Top demos

* [C64-DemoTop500.7z](/dl/C64-DemoTop500.7z) (70MB)
* [C64-OnefileTop250.7z](/dl/C64-OnefileTop250.7z) (5MB)

## Shortcuts

_Right Alt_ or _Right Ctrl_ +

```
O = Open fuzzy search
D = Swap disk
SPACE or N = Next file
P = Previous file
S = Change scaling
B = Change border
I = Toggle Info
T = Screenshot
U = Pause/Resume
R = Reset
C = Toggle CRT filter
W/SHIFT-W = Warp 10s/30s
J = Toggle Joystick/keyboard

For grid:

TAB = Next emulator
SHIFT+TAB = Previous emulator
ENTER = Maximize/Unmaximize
A = Select all emulators
SHIFT+N = Next file in all emulators

```

## Details

### File collection Logic

* Recurse all directories on the command line
* If _demo.m3u_ file found, that directory is added and not recursed
* If _disk images_ found in a directory, that directory is added and not recursed
* If _Amiga or Atari ST executables_ found in a directory, that directory is added
  and not recursed — the whole directory is loaded as a hard drive, so the data
  files next to the executable come along (`--many` splits it into single files)
* If other _executables_ found in a directory, each of the executables are added

### Windows demos

A Windows (PE) executable is run under [wine](https://www.winehq.org) inside a
fullscreen [gamescope](https://github.com/ValveSoftware/gamescope), on top of
demarc rather than inside it — the demo draws to the screen itself and plays its
own sound, while demarc sits behind it showing a black frame. Shaders, the grid
and screenshots therefore don't apply to it. Needs `wine` and `gamescope` on the
`PATH`; the wine prefix is `~/.wine-demos`, kept apart from the user's own
`~/.wine`, and wine creates it on first run. Stopping a demo closes that prefix
with `wineserver -k` — wine's service processes put themselves in sessions of
their own and outlive the demo otherwise, and they hold demarc's pipes open
while they do, which used to hang the quit. Starting one closes it too, since a
demarc that was killed rather than quit never got to: otherwise they pile up a
set per launch. Anything `wineserver -k` can't reach is killed by matching
`WINEPREFIX` in `/proc` — a wine process that outlives its own server (a wedged
`winedevice.exe`, usually) belongs to no server that could be asked to close it,
and stays until the machine goes down. So one Windows demo at a time.

Nearly every PC demo opens with a setup dialog, which nobody is there to answer,
so `demarc-autodlg.exe` (source in `tools/autodlg`) runs in the same wine
session, picks the resolution `wine_res` asks for and presses Start/Go/Run before
handing over. It also starts the demo itself, and writes a line when the demo
starts and another when it ends — which is the only way demarc can know either.
The process it launched is a gamescope, and gamescope waits on its whole tree:
wine's services outlive the demo inside it, so a session showing nothing looks
exactly like a session showing a demo. Without the driver's word for it a
finished demo goes unnoticed and keeps the screen. So the driver runs whatever
`wine_res` says; `pick` only stops it pressing anything.

`wine_desktop=true` runs the demo inside a wine virtual desktop
(`explorer /desktop=`) the size of the session. Demos switch display modes on
their way to fullscreen, and under gamescope's Xwayland that means tearing down
and remapping an X window, which a handful of them — Equinox's *Kings of the
Playground* among them — do not survive. Inside a virtual desktop the mode
switch is wine's own business and never reaches X. It's off by default, since
the desktop is a window manager of wine's own sitting between the demo and the
screen and most demos do better without one; turn it on for the ones that crash
or never draw.

`wine_res=pick` turns that off for a release: the dialog comes up untouched, for
you to answer with the keyboard and mouse — for the demo whose dialog the driver
reads wrongly, or the one with an option only a person can decide. The driver is
still there (`--no-go`), since it is what starts the demo and what reports its
end, but it presses nothing. Since the size isn't known until you choose it, a
pick session runs at 1920x1200, big enough to hold any mode such a dialog offers;
a demo started at less than that gets a picture that size in the middle of the
screen. The demo's window also keeps its title bar, undecorating being another
thing the driver is told not to do here.

The driver is a checked-in binary, so a change to `tools/autodlg` only reaches
demarc once it is rebuilt and `system/win/demarc-autodlg.exe` committed:

```sh
just autodlg
```

which cross-compiles it with `cargo-xwin`, the same way `just win` builds demarc
itself, and copies it into place. `+crt-static`, so the driver depends on
nothing but wine's own `kernel32` and `user32`.

### Tags

Tags configure the emulator per file. They come from a db header (`# Platform:Atari
puae_model:A500`) or line, an `.m3u`'s `#EXTINF`, or the command line
(`-x hatari_machinetype=ste`). Most are libretro core options (see `docs/flags.md`);
demarc adds a few of its own:

| Tag | Effect |
| --- | --- |
| `boot_file` | Which file in a release directory to auto start, e.g. `boot_file=TLKTLK2.PRG`. Overrides the guess demarc makes (named like a program — `.prg`, `.tos`, `.ttp`, `.app` — nearest the top of the release, and the biggest of those). Matched case insensitively, by file name or by path within the release (`DEMO/TLKTLK2.PRG`) |
| `psx_core` | `beetle` to load a PlayStation release with Beetle (needs a BIOS) instead of the default pcsx_rearmed |
| `wine_desktop` | `true` runs a Windows demo inside a wine virtual desktop, for the ones that don't survive a real display mode change (see above). Off by default |
| `wine_res` | Resolution a Windows demo is run at, e.g. `wine_res=1024x768`. Defaults to `800x600`, which is what the setup dialogs of the era all offer — the size is picked by matching the label on the dialog, so a size the demo doesn't list is one it won't run at. `wine_res=pick` leaves the dialog alone so you answer it yourself |

An Atari ST release directory is loaded as a hard drive, and the program is
started from the drive's `AUTO` folder. The release's own `AUTO` folder is moved
aside unless the started program lives in it — what a hard drive release keeps
there is usually the disk-swap loaders of its floppy version, which stop the boot
("insert disk 1 and reboot"). Such a release also defaults to a 4MB STE, since
nothing that needs a hard drive ran on a 1MB ST; `--ste`, `--xmem` and an explicit
tag still win.

### Command line arguments

```
Usage: demarc [OPTIONS] [FILES]...

Arguments:
  [FILES]...
          Path to the files to load, or an http(s):// URL to download and run

Options:
      --db <DB>
          Demo database file to load

      --many
          Treat disk images in same dir as separate files

  -s, --select
          Start with the file-open selector showing and load nothing automatically. Any files/dirs given are still collected and become the selector's list

      --scale <SCALE>
          How to map emulator screen onto window: `stretch`, `fit`, `zoom`, or a scale factor like `2` or `2.5` (fractional allowed)

          [default: fit]

      --border <BORDER>
          How to fill the border outside the image

          Possible values:
          - stretch: Stretch the edge pixels outward into the border
          - black:   Fill the border with background color

          [default: black]

      --shader <SHADER>
          Shader used to render the emulator screen. Defaults to the LCD shader for Game Boy / GBA titles and the Lottes CRT shader otherwise

          Possible values:
          - lottes:        Timothy Lottes CRT shader — scanlines/shadow mask, for CRT-era systems
          - lottes-simple: Single-pass WGSL port of the Lottes CRT shader — the pre-librashader path, sampling the emulator framebuffer directly
          - lcd:           cgwg dot-matrix LCD grid shader, for handheld LCD systems
          - lcd-simple:    Lightweight single-pass LCD grid shader (zfast-lcd)
          - none:          No post-process effect — render the raw emulator screen

      --slangp <SLANGP>
          Path to a libretro `.slangp` shader preset to use instead of `--shader`, e.g. any preset from the slang-shaders repo. Takes precedence over `--shader`

      --shuffle
          Shuffle the list of files into a random order

      --info <INFO>
          When to show overlay info text

          Possible values:
          - always:   Always show demo info on start
          - never:    Dont show demo info on start
          - on-multi: Show demo info on start with multiple files

          [default: on-multi]

      --aga
          Amiga: Force AGA (A1200 with 8MB Fast RAM)

      --ste
          Atari ST: Force STE

      --fast
          Amiga: Force high specs (68030 + FPU)

      --xmem
          Amiga/Atari ST: add extra memory

      --fast-load
          C64: Always use JiffyDOS to load
          Amiga: Turn off disk rotation emulation

      --silent-drive
          Amiga,C64,Amstrad: Dont produce disk loading sound

  -w, --window
          Open windowed instead of full screen

      --max-time <MAX_TIME>
          Max number of seconds to play a file before skipping

      --force-vsync
          Force vsync, slowing down or speeding up emulation to fit

      --speed-test
          Benchmark: run emulation unthrottled (no vsync, audio dropped) for two seconds, print the number of frames stepped, then exit

      --latency <LATENCY>
          Max queued frames. Lower values = better input response

          [default: 2]

  -x, --extra-options <EXTRA_OPTIONS>
          Extra options to add to libretro

      --grid <GRID>
          Render multiple emulators in a COLSxROWS grid, e.g. --grid=5x4

      --focus-first
          Start with the first grid cell maximized, as if it was the only emulator running. Un-maximize (RightAlt+Enter) to see the whole grid

      --clear-color <CLEAR_COLOR>
          Background clear color as a hex string, e.g. `#003` or `000080`

          [default: 000033]

      --reu
          C64: Add ram expansion unit (16MB)

  -C, --color-cycle
          ILBM: Animate colour-cycling (CRNG) ranges. Off by default

      --cbm-variant <CBM_VARIANT>
          Commodore variant (Only C64 well supported)

          Possible values:
          - c64:   Default Commodore C64
          - c128:  Commodore 128
          - dtv:   C64 DTV Stick
          - c16:   C16/Plus4
          - vic20: VIC 20

          [default: c64]

      --no-silence
          Don't silence libretro cores' stdout/stderr (for debugging)

  -h, --help
          Print help (see a summary with '-h')
```
