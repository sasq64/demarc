# RIFE frame interpolation

`--frame-convert=rife` interpolates the frames between two source frames with
[RIFE](https://github.com/nihui/rife-ncnn-vulkan) instead of the tile motion
estimation in `crates/frameconv`. Like the simple method, it shows any source on
a faster display (50 Hz on 60, 100, 144 Hz...) by stepping the source at an exact
ratio of the display rate (`frameconv::Cadence`).

## Building

RIFE is C++ on ncnn and Vulkan, so it is not part of the cargo build:

```sh
just rife    # clones external/rife-ncnn-vulkan, builds target/rife/librife_demarc.so
```

`external/rife-demarc/` wraps the `RIFE` class in a C API (`rife_new`,
`rife_process`, `rife_free`) that `crates/rife` loads with libloading. The
library is looked for at `$DEMARC_RIFE`, beside the executable, then in
`target/rife/`. Without it, or if RIFE cannot start, demarc exits with an error.

The model (`rife-v4.6`) is downloaded on first use into
`~/.cache/demarc/rife-v4.6`.

## How it runs

RIFE has its own Vulkan device and takes several milliseconds a frame, too long
for the render thread. `rife::Worker` runs it on a thread of its own, and
`src/frame_conv.rs` asks it for every phase of every source pair still in the
emulator's frame queue (`Emulator::conversion_ahead`), so the frames are ready
before they are shown. A frame that is not ready shows the source frame instead
(`frameconv: RIFE frame at .. not ready` at debug level).
