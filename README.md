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


## INSTALL

Pre-built binaries for Linux (x86_64), Windows (x86_64) and macOS (arm64) are
attached to every [release](https://github.com/sasq64/demarc/releases/latest).

Linux/macOS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/sasq64/demarc/releases/latest/download/demarc-installer.sh | sh
```

Windows:

```powershell
powershell -c "irm https://github.com/sasq64/demarc/releases/latest/download/demarc-installer.ps1 | iex"
```

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) (demarc is
not on crates.io, so point it at the repo):

```sh
cargo binstall --git https://github.com/sasq64/demarc demarc
```

Emulator cores are downloaded from the libretro buildbot on first use, so the
binary is all you need.

## BUILD

You need _rust_.

`cargo build --release`

On Linux the ALSA and udev headers are also needed
(`libasound2-dev libudev-dev` on Debian/Ubuntu).

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

## RELEASE

Releases are built by [dist](https://opensource.axo.dev/cargo-dist/)
(`dist-workspace.toml` + `.github/workflows/release.yml`). To cut one:

```sh
# bump `version` in Cargo.toml, then
just release-check          # sanity check what would be built
git commit -am "release: 1.3.1"
git tag v1.3.1
git push && git push --tags
```

Pushing the tag builds all three targets, then creates the GitHub Release with
the archives, checksums and the shell/powershell installers. `just release-local`
builds the current host's artifacts into `target/distrib` without touching CI.

After bumping the `dist` version in `dist-workspace.toml`, run `dist init --yes`
to regenerate the workflow.


