# ft2-clone as a libretro core

[ft2-clone](https://github.com/8bitbubsy/ft2-clone) is Olav Sørensen's
Fasttracker II clone. The `libretro/` directory in our fork turns it into
`ft2clone_libretro.so` the same way pt2-clone is turned into a core — read
[PT2.md](PT2.md) for how that works; everything there holds here too, with
`ft2_` in place of `pt2_`.

## Quick start

```sh
git clone -b libretro https://github.com/sasq64/ft2-clone libretro/ft2-clone
just ft2-core
just ft2 tune.xm
```

`FT2_CLONE_DIR` overrides where the tracker's sources are read from. In demarc
the `use_tracker` meta sends XM, S3M, STM, IT and the other formats the tracker
loads to this core, and ProTracker modules to pt2clone:

```sh
demarc -x use_tracker=true tune.xm
```

## Differences from pt2-clone

- The screen is 632x400 at 60Hz.
- MIDI input is not built (no `HAS_MIDI`), which also leaves out rtmidi and
  its C++.
- `SDL_CreateWindow` also turns off the config autosave, and sets the flag
  that skips the "not fully supported" box S3M, STM and IT stop at when
  they load.
- Copy and paste in text boxes go to a clipboard inside the core, not the
  host's.
- `chdir` is disabled on Windows as well, since the config lookup there
  would otherwise create an `FT2 clone` directory in the frontend's working
  directory.
