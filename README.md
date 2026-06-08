## demarc

<img width="3160" height="2370" alt="IMG_2029-high" src="https://github.com/user-attachments/assets/ca33d5ce-46a7-4f19-b0d2-a39ec551e05b" />

An command line emulator frontend for the demoscene

_because_

Emulation is better than youtube!


*Main goal*

Make it easy to watch demos from C64 and Amiga

* Runs multiple demos in order or shuffled
* Shows demo meta data as overlay
* CRT filter for "authentic" look (using Timothy Lottes shader)
* Can run Amiga/Atari/C64 exes & disk images
* Right-Alt hotkey for disk switch etc
* Can run multiple files at once in a grid


## BUILD

You need _rust_.

`cargo build --release`

## RUN

Set your monitor to 50Hz if possible.

then

`cargo run -- <files>`

or

`target/release/demarc <files>`

## SHORTCUTS

_Right Alt_ / _Right Ctrl_ +
```
D = Swap disk
N = Next file
S = Change scaling
B = Change border
I = Toggle Info
P = Screenshot
R = Reset
C = Toggle CRT filter
M = Click mouse
J = Toggle joystick/keyboard
W/SHIFT-W = Skip forward 10/30s

For grid:

TAB/SHIFT-TAB = Next/Prev emulator
ENTER = Maximize/Unmazimize
A = Select all emulators

```


