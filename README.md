## demarc

An command line emulator frontend for the demoscene

_because_

Emulation is better than youtube!

## Screenshots
**All screenshots are taken directly from Demarc in grid mode**

### Amiga Demos
`demarc --shuffle ~/Demo/Amiga --grid=6x5`

<img width="2880" height="1920" alt="amiga" src="https://github.com/user-attachments/assets/af18c9f8-aa7b-4d09-bbdd-7c031e337aff" />

### New Amiga/Atari ST Graphics

`demarc --db ../demodb/demozoo.txt -I category:Graphics$ --shuffle -I "author:(Steffest|Critikill|Slayer|Facet|Optic|Prowler)" --grid=9x8 -I date:202 -X platform:C64`

<img width="2880" height="1920" alt="graphics" src="https://github.com/user-attachments/assets/b92b06ad-60d9-43be-9e4b-29e826339ce1" />

### C64 Demos
`demarc -shuffle ~/Demo/C64/0* --grid=8x7 --fast-load`

<img width="2880" height="1920" alt="c64" src="https://github.com/user-attachments/assets/d0b2e9d4-e2d5-4b47-9693-1f0198936f6a" />

### GBA Cracktros
`demarc --db ../demodb/demozoo.txt --shuffle -I "platform:GBA" -I "category:Cracktro" --grid=5x5`

<img width="2880" height="1920" alt="gba_cracktro" src="https://github.com/user-attachments/assets/15a91673-7dfa-4abb-8100-f809d3512525" />

### New C64 Graphics
`demarc --db ../demodb/csdb.txt -I category:Graphics$ --shuffle -I "author:(The Sarge|Critikill|Facet|Prowler)" --grid=9x8 -I date:202`

<img width="2880" height="1920" alt="c64_graphics" src="https://github.com/user-attachments/assets/9cd0f1bd-7ceb-43e3-a5e7-dfbd9f0071bd" />

## INTRO

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
irm https://github.com/sasq64/demarc/releases/latest/download/demarc-installer.ps1 -OutFile "$env:TEMP\demarc-installer.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\demarc-installer.ps1"
```

The shorter `powershell -c "irm ... | iex"` one-liner does the same thing, but
antivirus and script policies tend to block that download-cradle pattern (it
fails with "Access is denied" before the installer prints anything), so
downloading the script first is the reliable route.

Both install to `%CARGO_HOME%\bin` (or `%USERPROFILE%\.cargo\bin`) and add it to
your PATH; set `DEMARC_INSTALL_DIR` to install elsewhere.

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

The tag must match the version in Cargo.toml exactly, prerelease suffix
included -- a `v1.3.1-rc.1` tag needs `version = "1.3.1-rc.1"`, otherwise dist
fails the run with "this workspace doesn't have anything for dist to Release".
Tags with a prerelease suffix are published as GitHub prereleases.

After bumping the `dist` version in `dist-workspace.toml`, run `dist init --yes`
to regenerate the workflow.


