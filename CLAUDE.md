# CLAUDE.md

## What this is

`demarc` is a command-line emulator frontend for watching demoscene
productions: it takes files, or a demo database, figures out which machine each
one belongs to, downloads/loads the right libretro core, and plays them full
screen through a CRT/LCD shader — optionally several at once in a grid. One
binary, Bevy 0.19 for the app/render loop, edition 2024, laid out as a Cargo
workspace — see *Crates* below.

## Important Notes

- When you make a mistake or unnecessary work due to wrong assumptions, record
  your learnings to docs/LEARNINGS.md. Always read this file first when doing work.

- Avoid long comments, and comments that describe current behaviour, even though
  there are many comments like that in the code.

- When given a coding task, prefer simple small changes. Fix problems if needed,
  but don't embellish (don't add information to the UI unless told).

## Commands

Standard cargo commands

Good profile for testing:

```sh
cargo build --profile release-fast    
```

## Testing changes

Run headless with

`<files> --headless --remote-control <lua_script>`

See `scripts/remote_example.lua`

*NOTE* Avoid running normally since it opens windows and plays audio.

## Architecture

### Crates

The repo root is both the workspace root and the `demarc` package, so `src/` is
demarc's own. The four crates under `crates/` hold the parts that do **not**
need Bevy, which is what lets the libretro layer be built for Android
(`docs/ANDROID.md`):

Dependencies point one way only. `demarc/src/main.rs` re-exports the moved
modules at its crate root, so `crate::backend`, `crate::newsys`, `crate::utils`
etc. still resolve inside demarc.

Two rules keep the split honest: **nothing under `crates/` may mention Bevy**,
and **`retro-core` may not gain a dependency that does not cross-compile to
`aarch64-linux-android`** (this is what keeps `utils.rs` out of it — `unarc-rs`
does not build for bionic).

## Conventions

- Unit tests live beside the code as a `mod tests` declared out of line: a
  `tests/` directory next to the source file, holding one `<module>_tests.rs` per
  module, pulled in with `#[cfg(test)] #[path = "tests/<module>_tests.rs"] mod
  tests;` at the bottom of the file
- A test that reaches repo-root content (`system/`, `testdata/`) uses
  `env!("DEMARC_ROOT")`, not `env!("CARGO_MANIFEST_DIR")` — the latter points at
  the crate. `DEMARC_ROOT` is set for the whole workspace in
  `.cargo/config.toml`.

## Docs worth reading before touching those areas

`docs/AMIBERRY.md` (Amiga core options/WHDLoad), `docs/86BOX.md`, `PICO8.md`,
`GAMESCOPE.md`, `PT2.md` and `FT2.md` (the non-buildbot cores),

## Releases

`dist` builds Linux/Windows/macOS artifacts when a `v<version>` tag is pushed.
The tag must match `version` in `Cargo.toml` exactly, pre-release suffix
included. See the RELEASE section of `README.md`.
