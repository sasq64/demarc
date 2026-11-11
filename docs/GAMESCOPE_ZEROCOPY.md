# Plan: zero-copy gamescope frames

Status: implemented. Where it differs from the plan below:

- There is no per-core installer. demarc registers a factory once, with
  `retro_core::frame_import::set_frame_importer` (`src/dmabuf_import.rs`, run at
  `Startup`), and a core gets an importer when it asks `DEMARC_DMABUF`.
- The ring reports `DRM_FORMAT_MOD_INVALID`, which is an implicit linear
  layout, not `DRM_FORMAT_MOD_LINEAR`. The core accepts both.
- `DEMARC_FRAME_STATS` also forces CPU frames, because its log needs every
  pixel.

Measured on RADV (Radeon 890M), headless, glxgears in a 1920x1080 session, as
demarc's own CPU time over 20 s: 10.9 s on the CPU path, 8.3 s on the GPU path
(23% less). On `testdata/atrium_by_tbc_and_loonies.zip` (1920x1080, 30 s from
10 s in, three runs each): 17.1 s on the CPU path, 12.2 s on the GPU path (28%
less). `convert_xrgb8888` was 15.8% of the CPU path's samples, and gathering the
diff samples is about 1% of the GPU path's. Screenshots of a still picture, alone and in a 2x2 grid, are
pixel-identical on both paths, and `video_level()` returns the same value. A
still session goes idle after `--idle-timeout`, and a moving one does not.
On NVIDIA (RTX 4070, driver 615) the import needs the `HOST_CACHED` memory
type (see `docs/LEARNINGS.md`). Atrium there, same method: 13.5 s on the CPU
path, 9.9 s on the GPU path (26% less), with the same frame pacing; a still
picture is pixel-identical on both paths.

## Problem

A Windows demo's frame travels like this today:

```
gamescope ring slot (dmabuf, linear, HOST_CACHED system memory)
  └─ core.cpp PublishFrame → video_cb(map[slot])
       └─ convert_xrgb8888            CPU copy #1 (+ BGRA→RGBA swizzle)   → Arc<Vec<u32>>
            └─ get_frame_diff         CPU read of up to 256K pixels
            └─ RetroUpdate channel → Emulator.frame_queue (delay line)
                 └─ upload_frames: queue.write_texture   CPU copy #2 → wgpu staging
                      └─ GPU DMA staging → emu.image (Rgba8Unorm)
```

`convert_xrgb8888` is ~6.6% in a profile of a demo run. At demo resolutions the
copy is memory-bound, so the swizzle itself is free: switching to a
matching texture format would turn it into a `memcpy` of the same cost. The only
way to win is to remove copies. Copy #1 is forced by the ring: `PublishFrame`
releases the slot as soon as `video_cb` returns.

## Goal

The GPU reads the ring slot directly. No full-frame CPU copies remain; the CPU
touches only the sampled pixels idle detection needs. Everything downstream of
`emu.image` (librashader, grid, dj, headless screenshots) stays unchanged.

## Design

```
gamescope ring slot (dmabuf)
  └─ imported once per slot as a wgpu Buffer (VkBuffer on the dmabuf memory)
  └─ core.cpp → demarc_dmabuf.present(slot)          [instead of video_cb]
       └─ core thread: gather sampled runs from the mapping → frame diff
       └─ core thread: compute pass  slot buffer ─swizzle→ pooled Rgba8Unorm texture
            └─ on_submitted_work_done → slot released back to gamescope
       └─ VideoFrame { gpu: Some(handle) } → channel → frame_queue (delay line)
            └─ upload_frames: copy_texture_to_texture  pooled → emu.image
```

Key decisions, with the alternative each one rules out:

- **Copy out of the slot on the GPU right away, into a texture demarc owns.**
  Sampling the slot itself from the shader chain would hold slots through the
  `MAX_LATENCY` (10) channel and the Bluetooth video delay line. The ring has 3,
  and gamescope drops a frame whenever none is free (`Present` returns 0). A
  GPU-to-GPU copy from VRAM is negligible; the read from system memory over PCIe
  costs the same as today's staging upload.
- **Import the slot as a buffer, not an image.** Importing a `VkImage` with the
  gamescope stride needs `VK_EXT_image_drm_format_modifier`, which wgpu-hal 29
  does not enable, and Bevy creates the device. wgpu-hal 29 does enable
  `VK_KHR_external_memory_fd` and `VK_EXT_external_memory_dma_buf` when present
  (`adapter.rs:1296`). A buffer import plus `Buffer::from_raw_managed` and
  `Device::create_buffer_from_hal` needs nothing else.
- **Swizzle in a compute shader that reads the buffer as `array<u32>`.** That
  handles any stride: `copy_buffer_to_texture` would need `bytes_per_row % 256
  == 0`, which an 800-wide linear image does not give on every driver. It also
  makes BGRA→RGBA and the alpha fill free, so the ring stays `XRGB8888` and the
  CPU fallback path is untouched. Making gamescope emit `XBGR8888` instead was
  rejected: the CPU path would then need its own swizzle, because libretro has
  no RGBA format.
- **Keep `retro-core` free of wgpu.** It gets a small trait object from demarc
  and only moves fds, slot numbers and an opaque handle. That respects the
  Android rule in `CLAUDE.md`.
- **Fall back to the CPU path for each frame, never for each session.** The
  indicator and black frames, frame conversion (RIFE/blend need CPU pixels), a
  GL backend, a failed import, `process.rs` cores and `DEMARC_NO_DMABUF=1` all
  take today's `video_cb` path. `VideoFrame` must handle a stream that mixes
  both kinds of frame.

## Phases

### 0. Baseline (go/no-go)

Use `perf` on one demo at 800x600 and one at 1920x1080 (see the
profiling-perf memory). Add up `convert_xrgb8888`, the `memmove` under
`write_texture`, and `get_frame_diff`. That total is the most this work can
save. If it is under ~5% at 1080p, stop here and lower `gamescope_resolution`
instead.

### 1. Spike: prove the import

Hard-code the path behind an env var, with no protocol changes yet. In
`video_refresh` for the gamescope core, `dup` the slot fds, which are already
in demarc's process because the core received them over `SCM_RIGHTS`. Then:

1. `vkGetMemoryFdPropertiesKHR` gives the memory type bits for the fd.
2. Create a `VkBuffer` with `VkExternalMemoryBufferCreateInfo(DMA_BUF_EXT)` and
   usage `STORAGE_BUFFER`.
3. `vkAllocateMemory` with `VkImportMemoryFdInfoKHR`, then bind.
4. Wrap it with `Buffer::from_raw_managed` → `create_buffer_from_hal::<Vulkan>`.
5. Run the compute swizzle into a texture and compare it with the CPU path's frame.

Questions the spike must answer:
- Does RADV accept a buffer import of memory that gamescope allocated as a
  *dedicated* image allocation? Vulkan only requires matching dedicated
  allocations for opaque fds, not dma_buf, but check it in practice.
- Is the result coherent without extra synchronisation? gamescope already
  `vulkan_wait`s before it sends `FRAME`.
- Are `RenderDevice` and `RenderQueue` reachable from the main world, and can
  the core thread submit work concurrently with Bevy's render thread? wgpu 29's
  `Device` and `Queue` are `Send + Sync`.
- Does it work on NVIDIA (`ssh ripper`)? If not, falling back is enough.

### 2. Protocol (`libretro/gamescope`)

- Add a private env call next to `GET_AUDIO_SAMPLE_BATCH_FLOAT`:
  `RETRO_ENVIRONMENT_DEMARC_DMABUF = RETRO_ENVIRONMENT_PRIVATE | 2`, carrying

  ```c
  struct demarc_dmabuf_iface {
      /* core → frontend */
      bool (*set_buffers)(unsigned w, unsigned h, uint32_t drm_format, unsigned n,
                          const int *fds, const uint32_t *stride,
                          const uint32_t *offset, const uint64_t *size);
      bool (*present)(unsigned slot);          /* false: send it via video_cb */
      /* frontend → core, filled in by the core */
      void (*release)(unsigned slot);          /* thread-safe */
  };
  ```

- `set_buffers` is called after HELLO, and again if the session restarts. The
  frontend `dup`s the fds.
- `PublishFrame`: if `present(slot)` returns true, do not `ReleaseSlot`. The
  frontend calls `release` once the GPU copy has finished. `release` is called
  from wgpu's callback thread, so it sends `GSLR_MSG_RELEASE` under the socket's
  existing send lock.
- Raise `GSLR_NUM_BUFFERS` to 4 and bump `GSLR_PROTOCOL_VERSION`. Slots are now
  in use by gamescope's write, the copy in flight, and the newest frame, which
  is held for `with_frame`. With 3, gamescope would drop frames regularly.
- Keep `IsBlack` and the indicator logic unchanged. They already read the
  mapping on the CPU.

### 3. `retro-core` plumbing

- `backend.rs`: give `VideoFrame` a `gpu: Option<GpuFrame>` field, where
  `GpuFrame = Arc<dyn Any + Send + Sync>`. Add `VideoFrame::same(&a, &b)` to
  replace the `Arc::ptr_eq(&pixels)` checks in `emulator.rs:1031` and
  `frame_upload.rs:58`.
- Add a trait set by the frontend:

  ```rust
  pub trait FrameImporter: Send {
      fn set_buffers(&mut self, bufs: &DmabufSet) -> bool;
      fn import(&mut self, slot: usize, done: Box<dyn FnOnce() + Send>) -> Option<GpuFrame>;
  }
  ```

  Install it on `RetroCoreThreaded` before load. `RetroCoreDirect::environment`
  answers `DEMARC_DMABUF` only when an importer is installed.
- Handle `present` on the core thread:
  - Mmap the slot (read-only, kept per slot) and gather the sampled runs into a
    small `Vec<u32>` between `DMA_BUF_SYNC_START` and `DMA_BUF_SYNC_END`.
  - Run the frame diff on those samples. Split `get_frame_diff` so that the
    sampling (`SAMPLE_RUN`, `sample_skip`) and the diff can run separately, and
    test that gathering then diffing gives the same result as `get_frame_diff`
    on full frames.
  - Call `importer.import(slot, release)` and put the handle in the
    `RetroUpdate`. `frame` keeps the last CPU frame so mixed streams still work.
- `with_frame` (`save_png`, cover art, remote-control screenshots): if the
  newest frame is a GPU frame, convert the newest held slot from its mapping
  with today's `convert_xrgb8888`. That costs one full copy per screenshot, not
  per frame. Access goes through a mutex shared with the core thread, so the
  slot is not released while it is being read.
- Add `RetroCmd::CpuFrames(bool)`, which makes `present` return false. The
  emulator sends it when frame conversion is switched on or off.

### 4. demarc importer (`src/dmabuf_import.rs`)

- Create it in `Emulator` setup from `RenderDevice` and `RenderQueue`. If
  `as_hal::<Vulkan>` fails (GL backend, headless without Vulkan), create none.
- `set_buffers` imports each fd as in phase 1 and builds the compute pipeline
  once. A small WGSL shader takes the stride in a uniform and writes `rgba` with
  `a = 1`.
- `import` takes an `Rgba8Unorm` texture (`STORAGE_BINDING | COPY_SRC`) from a
  pool, reused when its `Arc` is unique like the CPU frame pool. It records a
  dispatch, submits, registers `on_submitted_work_done(done)` and returns the
  texture as the handle. Bevy's per-frame poll fires the callback, so a slot is
  released no more than about one render frame late.
- `frame_upload.rs`: for a GPU frame, do `copy_texture_to_texture` from the
  pooled texture into `gpu.texture` in the render encoder, instead of
  `write_texture`. Skip it when the frame is the same as last time, as now.
- Drop imported buffers when the core unloads. Import closes its fd, and
  `from_raw_managed` frees the memory.

### 5. Verify and document

- Headless run with `--remote-control`: screenshot the same demo with and
  without `DEMARC_NO_DMABUF=1` and compare the images. Repeat in a 2x2 grid.
- Check that idle detection still ends a frozen session, using a demo that
  freezes on its last frame.
- Re-run phase 0's profile and record the numbers.
- Update the *Why it costs no extra copy* section in `docs/GAMESCOPE.md`.

## Risks / open questions

- **Driver support for dma_buf → buffer import.** Phase 1 settles this. If it
  is refused, an image import with a DRM modifier needs either a wgpu-hal
  change or creating Bevy's device manually (`RenderCreation::Manual`), which
  costs a lot more.
- **VRAM.** The pool grows with the delay line: up to ~20 textures at 8 MB for
  1080p. That is acceptable, but the pool should shrink when the queue drains.
- **Alpha.** The CPU path forces alpha opaque, so the compute shader must too.
  Otherwise PNGs and grid blending change.
- **Concurrent queue submits.** The core thread and Bevy's render thread submit
  in parallel. wgpu allows this, but submission order between them doesn't
  matter only because the pooled texture is written before the handle reaches
  the render world. Keep it that way.
- **Shape of the win.** This removes CPU work, not GPU work. A demo that is GPU-
  or wine-bound will not get faster frames, only a less busy demarc process.
