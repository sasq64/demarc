+++
title = "demarc"
template = "demarc.html"
+++



## Intro

_Main goal_: Make it easy to watch oldschool (PAL) demos

Supported systems:

C64, Amiga, Atari ST, Amstrad CPC, ZX Spectrum, Megadrive, SNES, Atari 2600, Atari XL, Tic-80


* Runs multiple demos in order or shuffled
* Shows demo meta data as overlay
* CRT shader (Lottes) for "authentic" look
* Can load disk images and executables
* Right-Alt/Ctrl hotkey for disk switch etc
* Can run multiple files at once in a grid
* Linux: Pause screen blanker and handle media keys


## Download (Windows)

Pre-built windows binary [here](/dl/demarc.exe)

*IMPORTANT:* Demarc downloads and links DLLs at runtime, which often makes Windows flag it as malware and silently delete it. Add an exception to your settings, or switch to a sane operating system. 

(Another note to windows users; if you _really_ don't want to use the command line, you can drag and drop demos onto the demarc executable to run them).


## Rust Install

You need [rust](https://rustup.rs).

`cargo install --git https://github.com/sasq64/demarc.git`

## Prepare

Set your monitor to 50Hz if possible.

## Run

`demarc --help`

`demarc <some_demo>`

`demarc --aga --shuffle Amiga/`

*TIP:* Download all intros from [https://intros.c64.org](https://intros.c64.org/) and run

`demarc --grid=4x3 --shuffle intros_c64_org_12596_full`

## Demo Packs

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
D = Swap disk
SPACE = Next file
S = Change scaling
B = Change border
I = Toggle Info
T = Screenshot
P = Pause/Resume
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

- Recurse all directories on the command line
- If _demo.m3u_ file found, that directory is added and not recursed
- If _disk images_ found in a directory, that directory is added and not recursed
- If _executables_ found in a directory, each of the executables are added

### Command line arguments

```
Usage: demarc [OPTIONS] [FILES]...

Arguments:
  [FILES]...
          Path to the files to load

Options:
      --scale <SCALE>
          How to map emulator screen onto window

          Possible values:
          - stretch: Fill the window, distorting the aspect ratio
          - fit:     Preserve aspect ratio, adding letterbox/pillarbox bars
          - zoom:    Preserve aspect ratio, cropping top/bottom or left/right to fill

          [default: fit]

      --border <BORDER>
          How to fill the border outside the image

          Possible values:
          - stretch: Stretch the edge pixels outward into the border
          - black:   Fill the border with background color

          [default: black]

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

      --window
          Open windowed instead of full screen

      --max-time <MAX_TIME>
          Max number of seconds to play a file before skipping

      --force-vsync
          Force vsync, slowing down or speeding up emulation to fit

      --latency <LATENCY>
          Max queued frames. Lower values = better input response

          [default: 2]

      --extra-options <EXTRA_OPTIONS>
          Extra options to add to libretro

      --grid <GRID>
          Render multiple emulators in a COLSxROWS grid, e.g. --grid=5x4

      --clear-color <CLEAR_COLOR>
          Background clear color as a hex string, e.g. `#003` or `000080`

          [default: 000033]

      --reu
          C64: Add ram expansion unit (16MB)

      --cbm-variant <CBM_VARIANT>
          Commodore variant (Only C64 well supported)

          Possible values:
          - c64:  Default Commodore C64
          - c128: Commodore 128
          - dtv:  C64 DTV Stick

          [default: c64]

  -h, --help
          Print help (see a summary with '-h')
```
