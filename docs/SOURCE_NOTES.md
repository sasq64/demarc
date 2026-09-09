# Source notes

Knowledge extracted from the comments in `src/` before they were thinned out. One heading per source
file, holding the parts that are not re-derivable from the code: design rationale, workarounds for
specific bugs in cores/libraries/platforms, format quirks, measured numbers, and the reasons behind
constants and orderings.

Companion to `CLAUDE.md` (which describes the architecture) — this file is the *why*.

## `main.rs`

Startup order matters: parse args **before** touching stdout/stderr, so clap's help/errors are
visible and `--no-silence` can be honoured when logging is set up.

**Muzzling the cores.** `silence_stdout()` (unix) dup2's `/dev/null` onto fds 1 and 2 so libretro
cores' `printf`/`fprintf`/`puts` vanish. The original stdout is duped first and that dup is never
closed — it is handed to `println()`, a `Write` targeting the raw fd directly (bypassing Rust's
`Stdout`), so the app can still print to the real terminal. It is `Copy` so a `MakeWriter` closure
can hand it out repeatedly without owning the fd.

**Process tuning, all done first thing in `main` while still single-threaded** (that is the SAFETY
argument for the `set_var`/`mallopt` calls), and each one leaves an explicit user setting alone:

- `raise_open_file_limit()` — soft open-file limit up to the hard limit.
- `tame_openmp_cores()` — keeps the OpenMP cores (bsnes, bsnes-hd, flycast) from eating the machine.
- `cap_malloc_arenas()` — `mallopt(M_ARENA_MAX, MALLOC_ARENAS)`. glibc's per-thread malloc arenas
  crowd the address space a JIT core (Amiberry) needs for its translation cache.
- `RAYON_NUM_THREADS` capped to match those arenas. The heavy rayon user is librashader, which
  compiles `.slangp` passes in parallel — glslang work that is nearly all allocation. Set via the
  env var rather than `ThreadPoolBuilder::build_global` so rayon stays out of demarc's dependencies.

**Bevy setup.**
- `WinitSettings::continuous()` — run the update loop regardless of window focus.
- `LogPlugin` is disabled; `main` installs its own tracing subscriber.
- `TaskPoolPlugin` caps the compute pool. Bevy's default grabs every remaining core and runs the
  multi-threaded ECS executor across all of them; this app has a handful of trivial systems, is
  GPU-bound, and has one dedicated emulator worker thread, so the extra threads spend their time on
  task-queue push/pop and mutex contention — ~34% of total CPU on a 24-core machine. Capping removes
  the spin at no throughput cost.
- `AssetPlugin` points at the extracted `system` dir, so assets ship inside the embedded `system.zip`
  rather than a loose `assets/` folder next to the executable.
- `RetroPlugin::fix_window` unconditionally forces `Windowed` at Startup (so early setup systems see
  a stable, non-transitional window size); `main` restores the requested fullscreen mode afterwards.

`fetch::prune_cache()` runs before anything writes into the caches, so this run's own downloads and
built discs can't be evicted out from under it.

`--sort rank`: ranks are positions so best-first; entries without a rank sort last via
`rank.wrapping_sub(1)`, keeping collection order.

Shader selection: an explicit `--slangp` wins; otherwise the bundled shader is resolved by name — a
`.wgsl` path selects the single-pass WGSL backend, anything else a `.slangp` preset through
librashader. `--shader none` starts with shaders disabled, but an explicit `--slangp` always enables.

## `config.rs`

clap `Args` (every doc comment is the `--help` text), plus `AppSettings`/`RenderSettings`.

- clap 4 drops the version from the help header, hence the hand-written `help_template`.
- `-I`/`-X` db filters are matched against **each field of the db line on its own**, so a pattern can
  pick on any one field but never spans two. `-I` repeatable and all must match; `-X` excludes on any.
- `--boot-file` is the command-line spelling of an override's `boot` key, for a local archive or
  directory with no db entry. Matched on file name alone (case-insensitive) anywhere in the release.
- `--scale` accepts `stretch`/`fit`/`zoom` or a factor (`2`, `2x`, `2.5`); whole numbers keep pixels
  integer-sized, fractional factors are applied exactly.
- `ShaderArg` is `Reflect` (the settings dialog reads its variant list off the type) and `PartialEq`
  (only a shader that actually changed re-points the render world, so an unrelated Apply cannot throw
  away a `--slangp`).

**`RenderSettings`** is the subset the render world needs and the *only* `ExtractResource`, so it is
the only thing cloned into the render world each frame — keeping the large `AppSettings` (and its
`files` vec) off the per-frame extract path. Hotkeys mutate it directly in the main world.

**`AppSettings::crt_limit`** — minimum magnification the CRT filter needs to stay on; below it the
effect is bypassed even when enabled, because scanlines/mask alias into mud at low magnification
(most visibly in grid mode). Applied per view, so the same core renders unfiltered in a small cell
and filtered once maximized. `0` disables the check.

**`override_for()`** — overrides are matched on the `id` a db line carries, so they only apply to
releases loaded out of a db; a file named on the command line has no id. The ids are demozoo's, so a
db from elsewhere can in principle collide. `--boot-file` is keyed on nothing, applies to every
release loaded this run (which is what makes it work on a local archive/dir), and being asked for by
hand it beats the file.

**`file_source`** — the picker's trigram search index, built lazily on first open and reused via a
cheap `Arc` clone; building it over the whole list is the picker's expensive step.

## `frontend.rs`

`RetroPlugin`: spawns one `Emulator` entity per view, lays out the grid, runs `run_retro`.

**Views are not cameras.** One `EmuCamera` composites everything (see `post_process.rs`); a view is
an entity holding `PostProcess` + `ViewRect`. A separate full-res UI camera sits on top — egui draws
into its pass, so the HUD lands over the emulators.

**`GridCell`** holds the view's rect as normalized `[0,1]` window fractions; `update_view_rects`
keeps `ViewRect` sized to it as the window changes. Each edge is rounded to a whole pixel, and
because adjacent cells share an edge fraction they round to the same pixel — so cells always tile the
window with no gap or overlap. A view *without* a `GridCell` fills the whole window.

With `--select`, the default `run_next` is cleared so nothing auto-loads, and the selector opens on
the first frame.

**Maximize**: the focused emulator fills the window and the rest stop drawing (`ViewRect::active`),
so it looks exactly like it was the only core running.

**Focus outline** (`draw_current_emu_outline`): orange, skipped when a single/maximized emulator
fills the window (it would just frame the screen) and when cross-fading (two emulators sharing the
whole window — same story). Gizmos are routed onto the UI render layer (layer 2) so they draw through
the full-res UI camera on top of the emulators. Camera2d uses logical pixels, origin centred, y up,
while cell offsets are top-left fractions with y down — hence the conversion. The rect is inset by
the line width so the outline sits inside the cell rather than being clipped at the edge.

**Input gating.** While the file picker or a controlled `TextList` is open (`hud.modal()`), all keys
are swallowed so they don't also reach the emulated machine. Mouse clicks are gated on `!modal` too:
the settings dialog is mouse-driven, so a click on its Ok button would otherwise also pick an
emulator — and as the second of two quick clicks, maximize it. The picker never hit this because it
is keyboard-only.

**Pointer mapping.** The OS cursor is mapped to normalized frame coords by inverting the same
letterbox transform the post-process shader uses (`source_uv = (screen_uv - uv_offset) / uv_scale`).
Pointer-driven cores (Flash) need this so the emulated cursor tracks the visible one. Hits in the
letterbox bars are ignored.

### `run_retro` (the main per-frame system)

- The frame check is a **read-only** probe first; the mutable borrow for the copy is taken only when
  the core has something new, because `get_mut` marks the asset modified on drop and Bevy answers
  that by re-uploading the whole texture.
- `frame_hash()` gates the copy: the screen refreshes at 60–165 Hz while a core produces 50–60 fps,
  and the threaded backend often has no update ready at all, so most passes have nothing new.
- The `AssetMut` is taken in a scope so its change-detection destructor releases the `images` borrow
  before it is taken again below.
- The texture is a byte buffer, the frame one packed RGBA `u32` per pixel — copied through a byte
  view (`backend::frame_bytes`).
- On a resolution change the texture is recreated as `TextureFormat::Rgba8Unorm` (raw display-space,
  **not** sRGB — see the note at the initial texture creation in `emulator.rs`), and `frame_hash` is
  reset to 0: the old texture's contents are gone, so a backend that isn't producing new frames (a
  still image, a paused core) would otherwise never refill it and stay black.
- Aspect is written through a guard (`PostProcess` is extracted to the render world; the aspect only
  moves on a video-mode change). There is a fudge: "for some reason we need to compensate the hatari
  aspect".
- `audio_active`: dropped entirely under `--speed-test`.
- Idle detection: when a timeout fires, `reset_idle` re-arms it along with the request. A core that
  has gone idle stays idle while the next release downloads, so leaving the baseline alone would set
  `run_next` again every frame — starting a fresh load each time until the download lands.
- `load_async` takes `run_next`/`run_prev` as it starts, so a load fires once per request rather than
  every frame of a long download — and a request arriving *during* one (selector, hotkey) still gets
  through and replaces the load in flight. The previously loaded core keeps running until the new one
  lands, so a slow mirror no longer freezes the picture.
- The warp/skip indicator is taken down the moment the skip is over rather than on a fixed timeout;
  an empty toast text retires whatever is in that corner (see `spawn_toast`).

## `emulator.rs`

The `Emulator` component: one backend instance rendered into its own `image` texture, plus pacing,
input routing, audio sink and the async load state machine. Several coexist as separate entities.

**Texture format is `Rgba8Unorm`, NON-sRGB on purpose.** libretro cores deliver raw display-space
(gamma-encoded) pixels, exactly like a RetroArch core framebuffer. The `.slangp` shaders do their own
gamma linearization, so `Rgba8UnormSrgb` here would make the hardware sRGB-decode the frame before
the shader and double-linearize it. (Matching note on the output side lives in `blit.wgsl`.)

**`frame_hash`** is `Backend::frame_serial` as of the last copy; the display refreshes far faster than
a core produces frames, so this is what keeps `run_retro` from re-uploading identical pixels.

**Input.** `InputMode` cycles Keyboard → Joystick1 → Joystick2. It only re-routes the cursor keys and
Enter (to the d-pad + fire of libretro joypad port 0/1); every other key keeps going to the keyboard
even in a joystick mode. The absolute pointer is sent *after* the relative motion so it is
authoritative for cores that track an absolute cursor (Flash); relative-mouse cores ignore it.

### Async loading

`load_async` returns immediately. Download **and** unpack run on the I/O pool, so neither a slow
mirror nor a big archive is paid for on a frame the previous release is still drawing; only what
touches shared state — system detection, conversion, core creation — is left for `load_prepared` on
the main thread. `update_load` polls once per frame and hands the finished `WorkFile` to
`load_prepared`, so the caller sees exactly the outcome the old synchronous `load` produced, some
frames later.

- A load already in flight is **abandoned** on a new request (and its temp dir discarded), which is
  what makes picking another entry mid-download take effect instead of queueing. The abandoned job
  never reaches `update_load`, so its share of the in-flight download counter is given back at the
  abandon site.
- `run_next`/`run_prev` are **taken**, not read, when the load starts — leaving them set would have
  the frontend request the same load again next frame. A load that *fails* puts them back
  (`failed_load`), which is what lets tv mode carry on past a dead link in the direction it was
  already going, while an interactive session clears them itself and stops on the error.
- `PendingLoad::info` carries the title because on failure there is nowhere else to read it from —
  `work_file` still describes whatever was loaded before.
- `JobError::Failed` is unwrapped rather than wrapped: `load_error::classify` downcasts along the
  error chain to tell a 404 from a dead mirror.
- The one part of an override applied *before* the transfer is `download` (which of the release's
  files is the demo); `boot` and patch files are applied after, so the `Override` is held in
  `PendingLoad`.
- Progress switches back to indeterminate (`set_total(0)`) for the unpack phase, so a progress bar
  doesn't sit at 100% for the rest of the job. The byte-counting plumbing exists end to end
  (`fetch` counts, `FileSource::resolve_with_progress` forwards) but the UI only draws the count of
  downloads in flight, so `load_progress`/`is_loading` are `#[allow(dead_code)]`.
- `load()` (synchronous, dead code) is kept as the one place the synchronous order of a load is still
  written out.
- `LOAD_SETTLE_SECS = 0.1` — roughly the handful of frames this used to be at 60 Hz, no longer tied
  to frame rate.

**`load_prepared` drops the old core (`self.core = None`) *before* building the new one.** A backend
may own something the machine only has one of and the next cannot take it until this one lets go.
The case that bites is `musix`'s sc68 plugin: libsc68 has a process-wide init the plugin claims per
song, so a second SNDH loaded while the first is alive fails to init and no plugin is found for the
file — but libretro cores are widely non-reentrant the same way. The cost is that a failed load
leaves nothing running rather than the previous entry; the frontend already draws that (it skips an
emulator with no core) and tv mode steps on.

### Pacing (`run`)

`display_fps` is an EMA of measured frame time (seeded only from a plausible 40–500 fps reading).
When the core's fps and the display's agree within 1% for 8 consecutive frames it latches
`match_fps` and simply steps one core frame per display frame; otherwise it catches up against
`next_frame` on wall-clock time.

- `speed_test`: pump the core once per update, no audio handling, no pacing — throughput bound only
  by CPU/GPU.
- `occupied_len > AUDIO_BUF_MAX` → drop a frame.
- `occupied_len < AUDIO_BUF_MIN` → run an extra frame to refill. Gated on `has_audio`, which is
  `audio_seen` — **samples actually arriving**, not the core advertising a sample rate. A core can
  report 44.1 kHz and emit nothing for a silent ROM; treating that as "has audio" makes the
  buffer-dry check fire every frame and runs the demo at double speed.
- A skip delivers no audio at all while it runs (the worker discards it and sends no updates), so the
  first samples afterwards mark its end. Clearing `skipping` there re-arms the buffer-dry catch-up,
  which is the only thing that refills the ring buffer the skip drained; left latched, the deficit is
  never repaid and the audio callback underruns from then on. `audio_rate_adjust` (the controller's
  integral) is reset at the same point — it has wound down to its clamp against an emptiness the
  controller had no say over.

**`reset_idle`** re-arms the idle timer by hand after a timeout has been acted on: the core goes on
being idle while the next release loads, so otherwise the timeout fires again next frame. It sets
`idle_time` too, because `run` only recomputes it when there is a core and there is none mid-load.

**`skip_finished`** is true on the first frame after a warp the indicator was shown for has run out.
The threaded backend reports the skip started synchronously so no "not started yet" grace period is
needed; every other backend finishes its skip inside `skip()` and so reads as done next frame, which
is right. `warp_shown` is latched even with no core to skip, so the indicator comes down next frame
instead of sitting out its timeout.

## `backend.rs`

The `Backend` trait — the only thing the frontend knows about a "core" — and the shared frame
representation. Deliberately free of any libretro dependency: libretro cores are one implementation
alongside image, music, Flash and Wine backends.

- `ViewFocus`: `Invisible` (another view maximized over this one) / `Visible` (a grid tile, not
  selected) / `Current` (exactly one at a time, maximized or not). A backend that runs just as well
  unwatched ignores `focus`; the music backend uses it to stop rendering audio nobody hears.
- `STATE_SKIPPING = 1 << 0` — set while fast-forwarding through `skip_frames`, cleared on the frame
  the skip runs out. `state()` is read every displayed frame so it must be cheap (an atomic load for
  the threaded core, the only backend with anything to report).
- `frame_hash()` has **no default implementation on purpose**: the frontend re-uploads the texture
  only when it moves, so a backend leaving it constant is never redrawn. Any monotonic counter or
  content hash will do — it only has to differ, not increase.
- `set_mouse_position` is normalized frame coords, origin top-left. libretro cores driven by relative
  mouse motion ignore it; Flash needs it so Ruffle's internal cursor tracks the visible OS cursor for
  hit-testing buttons.
- `send_keys(&[(frame, keycode)])` — frame is relative to now (`0` = next stepped frame), each key
  released two frames after press. Used for a core's "startup keys".
- `get_number_of_disks` takes `&mut self` because the libretro implementation calls into the core,
  which may issue environment callbacks while it does.
- `frame_bytes()` reinterprets packed RGBA `u32`s as bytes; each `u32` already holds `[r,g,b,a]` in
  memory order (see the LUTs / `video_refresh`), so it is a plain, always-sound width narrowing.

## `post_process.rs`

Compositing of every emulator view into one camera, via librashader or a single-pass WGSL shader.

**Backends.** `ShaderEffect::Slangp(path)` runs a librashader `.slangp` filter chain into an
intermediate texture that a *passthrough* composite blit (`shaders/blit.wgsl`) then draws. With the
effect toggled off there is no chain at all — the composite samples the emulator framebuffer
directly. `ShaderEffect::Wgsl(asset)` is the older single-pass path: one WGSL shader samples the
framebuffer directly and applies the effect in the composite pass, toggled in-shader via
`crt_enabled`.

**Formats.** `TARGET_FORMAT = Rgba8UnormSrgb` for both the librashader intermediate and the composite
output; matches Bevy's view-target main texture (formerly `TextureFormat::bevy_default()`).
The emulator source itself is `Rgba8Unorm` — sampling it directly is marginally *more* accurate than
going through the sRGB intermediate, which encodes and re-decodes every texel (up to 1/255 on bright
values).

**Bevy 0.19 rendering.** The render graph is gone; a render pass is a system in the per-camera
`Core2d` schedule. `post_process_pass` runs in the `PostProcess` set (where tonemapping lives),
ordered before upscaling presents to the swapchain. It is invoked once per camera with `CurrentView`
set, and its `ViewQuery` skips every camera but the one marked `EmuCamera` (notably the UI camera,
which draws the HUD on top afterwards).

**`ShaderPath` resource** is `ExtractResource` *and* inserted into the render world directly, because
`RenderStartup` runs one extract too early to see it otherwise.

**One camera, many quads.** Grid mode used to give every cell its own `Camera2d` with a viewport, so
the whole 2D pipeline (extraction, view uniforms, main pass, tonemapping, upscaling) ran once per
cell. Now a grid is a single camera and each view is a plain entity contributing one quad;
`ViewRect` is what that per-cell camera viewport used to be, written from the window size by the
frontend. `ViewRect::active == false` means another view is maximized over this one — the quad is
skipped, exactly as an inactive camera used to be.

**Scale modes.** `scale_offset()` returns `(uv_scale, uv_offset)`; the shader (and the pointer mapping
in `retro.rs`) map screen-uv to source with `(screen_uv - uv_offset) / uv_scale`. `Stretch`, a
degenerate size, or a target already matching the source aspect all give the identity transform.
`Fixed(n)` does exact integer scaling, aspect-aware: the core's reported display aspect divided by
the framebuffer's pixel dimensions gives the pixel aspect ratio (PAR) — Game Boy 160×144 ≈ PAR 1 →
n×n; an Amiga half-width framebuffer → PAR≈2 → 2n×n; half-height → PAR≈0.5 → n×2n. The denser axis
keeps exactly `n`, the other is multiplied by PAR and rounded so pixels stay integer-sized. A
*fractional* factor (2.5) is an explicit request for non-integer scaling and is honoured exactly on
both axes.

**Border handling.** `BorderMode::Stretch` = `ClampToEdge` (edge texels stretch into the bars);
`BorderMode::Black` = `ClampToBorder` with a black border colour. `ClampToBorder` is a native-only
wgpu feature — Bevy's default `Functionality` priority enables it on desktop, but the code checks and
falls back to edge-clamping rather than panicking. `BorderScissor` restricts the blit to the image
rect so the bars keep the camera clear colour instead of being overdrawn; `None` means no clipping
(used by `Stretch`, and whenever the image fills the viewport under `Stretch`/`Zoom`). A scissor is
always set anyway, or the previous quad's scissor would clip this one. Everything is clamped to the
framebuffer: wgpu rejects a viewport or scissor poking outside it, and a stale `ViewRect` written
before a resize can do exactly that.

**Per-frame cost.** `update_post_process_uniform` runs every frame but almost nothing ever changes, so
both writes go through `set_if_neq` and components are only inserted when missing — an unconditional
write would mark them changed and re-queue an archetype insert through `Commands` every frame.

### Seam / moiré fixes (three separate ones, don't confuse them)

1. **Integer-snapped composite transform.** `post_process_pass` sizes the intermediate to
   `inter_size = round(viewport * uv_scale)`, but the image's on-screen footprint is the *fractional*
   `viewport * uv_scale`. That sub-texel mismatch makes the nearest-sampled blit drift up to half a
   texel and, by centring symmetry, duplicate one column at the exact screen centre — the thin red
   vertical line under crt-lottes at "Fit". Forcing the footprint (and top-left) to integers makes
   `(screen_uv - uv_offset)/uv_scale` land on exact texel centres.
2. **Intermediate sized to on-screen extent.** The effect is rendered at exactly the display pixel
   density it will be shown at, so the blit samples 1:1 instead of bilinearly resampling a fixed-size
   effect — that resampling beats against the high-frequency phosphor mask and produces coloured
   R/G/B moiré lines (worst under Stretch/Zoom). For `Fit` `inter_size` is the aspect-correct image
   rect; for `Stretch` the full rect; for `Zoom` larger than the rect on the cropped axis.
3. **crt-royale triad nudge.** crt-royale tiles its mask at a fixed triad size (default 3px → 24px
   tiles). When `render_width / tile_size` is an even integer a tile boundary lands exactly on screen
   centre, where the mask's manual `frac()` tiling has a coordinate discontinuity that duplicates a
   subpixel (visible red vertical line). crt-royale's own `FIX_DISCONTINUITIES` uses `ddx/ddy` in a
   vertex-shared header and won't compile on the slang/glslang path. Instead pick the integer tile
   size near 24px whose centre offset is furthest from a boundary — triads stay ~3px, mask stays
   pixel-sharp, seam moves off-centre. No-op on presets without the parameter.

### Downsampling (`--downsample`)

Minification — the on-screen image smaller than the source on at least one axis — is the one case the
CRT/LCD presets cannot help with: no spare display pixels for a scanline or mask to live in, and
squeezing the frame drops source pixels outright, which aliases. `DOWNSAMPLE_PRESET`
(`shaders/slangp/downsample/drez_1x.slangp`) replaces the effect there; it band-limits as it
resamples. `wants_downsample` is taken **per axis**, not on the aspect-corrected magnification,
because a frame stretched on one axis and squeezed on the other (a half-width Amiga screen at 1:1)
still aliases on the squeezed one. `limit <= 0` never downsamples; the default `1.0` is exactly "the
view throws source pixels away". It is independent of `crt_enabled` — `crt_limit` has almost always
switched the effect off by then, and this is a resampler, not a look.

`pixel_ratio()` (used for `crt_limit`) instead takes the **minimum** of the two axes: the effect is
limited by whichever axis has least room.

**Per-view effect decision.** `crt_enabled` is computed once in `compute_uniform` (global
`crt_effect` gated by `crt_limit` against *this view's* magnification) and read back off the
extracted component by the slangp backend, so both backends agree on one decision. In grid mode the
answer differs cell to cell. When the source isn't loaded yet, keep the global setting rather than
flickering the effect off for a frame.

**Skipping the chain.** If a view wants neither the effect nor the downsampler, the chain would be
`stock.slangp` — a nearest-sampled verbatim copy. Composite the framebuffer directly instead:
`blit.wgsl` maps uv and linearizes identically, and this saves a full-screen pass, the intermediate,
and (in grid mode, where `crt_limit` switches most cells off) building any chain for that source at
all.

**Pass ordering.** All filter-chain work happens *before* the composite render pass is opened —
chains record into the same command encoder and a render pass may not be live while they do. Bind
groups are built while the chain's intermediate is still borrowed and drawn from later; a `BindGroup`
is a cheap handle, whereas holding a `TextureView` borrow would keep `chains` locked for the whole
pass.

### Chain building (`SlangChains` / `AsyncChain` / `Build`)

Building a chain is expensive and unbudgeted: a Mega Bezel preset is 42 GLSL passes and glslang alone
takes ~1.5 s of CPU to make SPIR-V (naga + wgpu pipelines ~0.1 s, the 32 LUT images ~0.1 s — the
images are *not* the cost). Doing that inline on the render thread froze the app for seconds on a
shader change, so it runs on `AsyncComputeTaskPool` and nothing ever waits: until the new chain
lands the view keeps rendering the previous one, or composites unshaded if there is none. wgpu's
device and queue are refcounted handles and resource creation from another thread is supported, so
the whole load runs on the worker (including the `device.poll(Wait)` librashader ends with, which
blocks that worker only). The task body is not async in any real sense — nothing awaits, the
blocking compile just occupies a pool thread.

**Coalescing.** At most one build per source is in flight. A superseded build is flagged `cancelled`
(checked before it starts, so a build queued behind others costs nothing) but glslang cannot be
interrupted once it has the shaders, so a running one finishes and its result is dropped. Starting
the next selection alongside it would just have them fight for CPU, and with a click per preset the
one the user settles on ends up last behind a queue of abandoned work. `BuildResult::Cancelled` is
kept distinct from `Failed` so the preset is not blamed. Failed presets are remembered in
`SlangChains::failed` (shared across sources — a broken preset is broken for all) so they are neither
retried nor re-logged.

**Chains are per-source, deliberately not shared.** A single `FilterChain` keeps internal per-pass
framebuffers sized to its last `frame()` call; feeding differently-sized sources through one within a
frame (a grid of images at different resolutions) latches the last size and resamples the next cell
through it — e.g. an 18×18 brush in the grid blurs every other image. `FilterChainWgpu` owns clones
of the wgpu `Device`/`Queue` and is `Send`/`Sync`, so `SlangChains` lives as a render-world resource.

`frame_count` (fed to shaders for feedback/animation) only ticks on frames that actually run a chain,
so an animated preset resumes where it left off instead of jumping after a spell with the effect off.

`set_effect` repoints only the effect chains; the downsample preset comes from `--downsample` and
stays valid across a shader change. `init_filter_chains` runs even on the WGSL backend (which builds
nothing) because the settings dialog can switch to a `.slangp` preset later.

Composite pipelines are keyed by shader asset path and queued lazily on first use — normally exactly
one entry; the only way to gain another is picking a different backend in the settings dialog, and
there are three shaders total.

Misc: `SamplerBorderColor` isn't re-exported by Bevy, pull it from `wgpu` directly.

## `newsys.rs`

The load pipeline and the `System` trait.

**`System` trait contract** (verbatim intent from the trait docs):
- A System identifies, converts, configures and loads releases for a particular machine (or family).
- The frontend first downloads, unpacks and does non-system-specific conversion. The result is a
  folder/file combination where either (but not both) may be `None`, and the folder must be a parent
  of the file. Existing m3u files are handled by the frontend and never passed on to loading.
- The result is passed to each system in order; the first that succeeds is used. **Ordering is
  priority** for uncertain detection.
- `load()` may change the `WorkFile`; on success it can be handed to `create()`. On failure the
  `WorkFile` is assumed unchanged. The default implementation returns the first file it "can load".
- `is_console()` → true for systems that default to gamepad control.
- Examples: `demo.t64` — C64, extension unique. `demo.m3u` — never passed to a system; the frontend
  extracts meta and passes the directory instead. `demo.cue` — systems must parse the cue and look at
  the bin/iso to tell PSX from Neo Geo; if uncertain prefer the more common PSX, and meta can
  pre-decide.
- Directory (the most common case): single valid file → sort files in priority order and pick the
  first (PRG over IFF over PNG, CUE over ISO); disk images → collect, sort and write an m3u (Amiga
  multi-URL download); HDD (Amiga/Atari) → detected by an EXE file, optional startup-sequence.
- Meta: the frontend merges argument meta with m3u tags first; the system then adds default meta not
  already set, plus meta depending on content.
- `Send + Sync` so `Box<dyn System>` (and `NewSys`) can live in a Bevy resource. All implementors are
  plain data so it costs nothing, and it keeps the module free of any bevy dependency.

**Windows system is `#[cfg(target_os = "linux")]`.** wine and gamescope are Linux-only, so elsewhere
a `.exe` with a `PE` image is something nothing here can run — and claiming it would take the release
away from the picture/music systems that can at least show what shipped alongside the program.

**System order gotchas**: `Plus4System` is registered before the C64, which would otherwise claim the
same disks and programs; it stands aside unless `--cbm-variant` asked for it. `MusicSystem` and
`ImageSystem` go last (musix and `image` claim a lot of files).

**`unpack_release` / `load_prepared` split.** `unpack_release` is the one expensive step that touches
no shared state — it reads the path and writes into a temp dir of its own — so the frontend runs it
on the I/O pool while the release on screen keeps playing. On the main thread it cost a visible
stutter exactly where it is least wanted: a double-packed release is unpacked twice, and that landed
on a single frame. What is left for the main thread (detection, conversion,
building the backend) either needs the system table or *is* the core. Archives are unpacked one level
deep and then once more, because scene releases are routinely packed inside another archive. An m3u
is not unpacked at all: its tags become meta and the directory it names is what gets loaded.
`load_file` = the two in order, kept as the one place the whole pipeline is written out (tests only).

**Meta precedence in `load_prepared`**: the release's own meta, then the override (which beats it),
then `self.meta` (`-x` on the command line) applied **last** so it beats every other source.

**Disc handling before any system looks at the file** (a directory holding a cue and its tracks is
handed over one file at a time):
- A cue sheet that can't find one of its files is unloadable — no core will open it — so step over it
  to the directory it sits in and let the systems find the image themselves.
- Prefer the sheet over a bare track: the track on its own leaves the disc's CD audio behind.
- A cue's MP3 audio tracks are unplayable to every core here (they read the compressed bytes straight
  through as PCM), so the sheet is rewritten with those decoded before the core opens it. A disc that
  needs nothing comes back untouched; a sheet that can't be rewritten is still handed over as-is
  because the core may make more of it than we do.

**`apply_override`** — parts are independent and any may be absent. `fast` goes on **first**, because
it is a whole Amiga configuration written as one word (`amiga::apply_fast`), so an entry that also
names an option of its own has that one stand. `boot_file` narrows the work file's path to one
program, which skips the systems' own file picking — and sets `RELEASE_DIR` meta, because a system
tells "one loose program the user pointed at" from "a whole release" by whether the work file is a
file or a directory and copies data files along only in the second case (`copy_all` in
`newsys::amiga`); narrowing the path would otherwise throw that away. A `boot` naming something not
in the release is a warning, not a failure: falling back to the systems' own pick still runs
something.

**`apply_patches`** adds or modifies files in the release directory — callers must `make_temp()`
first. `write_patch` replaces the file entirely when there is no offset, overwrites at `offset` when
there is, and zero-extends a file too short to reach it.

**`set_meta`** (how the settings dialog moves `latency`) is applied in `load_prepared`, so it takes
hold on the *next* release loaded, not the one playing: a backend reads its meta once, as it is built.

**`prune_caches`** runs once at startup alongside `fetch::prune_cache`, for the same reason: nothing
holds a path into any cache yet, so this run's own work can't be evicted. Each cache carries its own
budget because entry cost differs by two orders of magnitude between them.

`walk_dir` / `walk_dir_find` — walk a dir with (path, lowercased extension, first `header_size`
bytes); the `_find` variant stops on the first `Some`.

Open TODOs left in the code: "We should not collect m3us"; "Maybe insert `grid` and let core decide?"
(next to `psx_core = beetle`).

## `overrides.rs`

Per-release fixups read from `overrides.toml` at startup, keyed on the **demozoo id** (the `id` field
of a demozoo db line). A db line says where a release can be downloaded and little else; for a
handful of releases that isn't enough — the listing holds three files where only one is the demo, the
archive holds two programs where only one is the one to start, or the release needs a config file it
was never packed with.

```toml
[zoo.102]
file = "rgba_tbc_elevated.zip"      # which download to fetch
boot = "elevated_1280x720.exe"      # which file inside it to start

[zoo.68604]
libretro = { dosbox_pure_cycles = "max" }   # core options, as meta

[zoo.18030]
file = "inside.zip"
patch = { target = "SOUND.CFG", contents = "U0RJR1VT…", info = "GUS 0x240" }

[zoo.119665]
assign = { Love = "SYS:" }         # AmigaDOS assigns to make before booting

[zoo.7236]
fast = true                        # accelerated A1200 with FPU, Z3 mem and JIT
```

Every key is optional; several patches go in an array (`[[zoo.<id>.patch]]`). The three stages of a
load each take their part: `file` when it is downloaded (`FileSource::pick_download`), `patch` once
unpacked, and `boot`/`libretro`/`fast` as it is handed to a system (both in `NewSys::load_file`).

- `search_paths()`: the directory demarc was started in → the user's config dir → the system dir the
  bundled assets are extracted to. First that exists wins, so a file in the working directory is how
  you try a new override without touching the installed one.
- Overrides are a convenience, not a requirement: no file is the normal case (empty map), and a file
  that doesn't parse is reported and ignored rather than taking the run down.
- Unknown top-level keys are kept only so they can be warned about — a mistyped `[zoo_57849]` is a
  table of its own as far as toml is concerned, and silently doing nothing is the least helpful
  response.
- One bad entry (non-numeric id, base64 that doesn't decode) is reported and dropped on its own; the
  rest of the file still applies. `contents` is decoded at startup and thrown away, so a typo is
  reported then rather than by the one load that needs it.
- `patch.offset` left out replaces the file entirely, which is what a small config file wants.
- `assign` is flattened to `Name=Target;Name2=Target2`, the shape `newsys::amiga::handle_exe` splits
  back apart — written out because meta is strings all the way down.
- Every string is leaked on the way (`files::leak`): that is what `Override` and the entries it is
  applied to hold, and the file is read once at startup and lives for the run anyway.
- `meta_value` accepts numbers and bools unquoted (`dosbox_pure_cycles = 150000` is written that way
  often enough that rejecting it would be pedantic); a table or array is a mistake.

## `newsys/amiga.rs`

**Cores.** `puae` and `amiberry` (`CORE_NAME_AMIBERRY = "amiberry"`). The libretro buildbot does not
ship amiberry, so it comes from its own release via `ALT_SOURCES`, or `$DEMARC_CORE_DIR` for a local
build (see `docs/AMIBERRY.md`).

**`amiga_system_dir()` is `system/amiga/`, a subdirectory, not `system/` — this is load-bearing.**
Amiberry's startup ROM scan walks the directory it is given *recursively*, opens every file, and
probes it for an Amiga ROM by content — and the probe treats anything carrying an archive signature
as an archive, whatever the file is called. `system/` is shared by every core, so the scan used to
reach vice's, musix's, PCem's and Ruffle's data:

- PCem's AMI BIOS images (`system/pcem/roms/430vx/55xwuq0e.bin`) carry `-lh5-` at offset 2, because
  that is how AMI packs its modules — amiberry's LHA decoder then overruns a stack array unpacking
  one. The process died in `__stack_chk_fail` inside `lha_make_table()` before `retro_load_game`
  returned, taking demarc with it. Nothing on our side can catch an `abort()` in a core, so the fix
  is to not show it the file.
- The scan CRC32s and SHA1s all ~1000 files — the ~2 s per demo start recorded in `docs/AMIBERRY.md`.

puae doesn't rummage (it looks files up by name) but reads the same Kickstarts, so it gets the same
directory. Amiberry also *writes* there (`amiberry.ini`, `Configurations/`, `Savegames/`, WHDLoad
save data), which is why it is a real directory in `system/` rather than assembled per run.

### AmigaDOS executable detection (`is_amiga_exe`)

`HUNK_MAGIC = 00 00 03 F3` (`HUNK_HEADER`) alone says very little — old BBS packs are full of files
that start with it but are truncated or mangled, and picking one over the real demo next to it means
booting into nothing. So the whole hunk stream is walked the way `LoadSeg()` does and anything that
doesn't hold together is rejected. Only longwords are read and data is seeked over, so it stays cheap
on multi-megabyte executables. `HunkReader` never reads or seeks past EOF, so a truncated file fails
instead of quietly running out of blocks.

Details that came from real releases:
- `MEM_MASK = 0x3FFF_FFFF` — the top two bits of a hunk size (and of a hunk id) select the memory
  type; memory type `11` means an explicit attribute longword follows.
- Relocation tables are `(count, hunk, count * offset)` groups ended by a zero count; offsets are
  longwords in the 32-bit hunks and **words** in the `DREL` ones, where the table is padded out to a
  longword. Resident library names are longword-counted strings ended by a zero length.
- `LoadSeg()` reads one block at a time and skips blocks carrying no memory wherever they appear, so
  a hunk may open with debugger or name blocks before its contents — `eph-fels.exe` leads with a
  `HUNK_DEBUG`.
- A block may ask for **less** memory than the header reserved (crunchers do that for scratch space)
  but never more.
- `HUNK_END` is what `LoadSeg()` waits for, not what it needs: a hunk is finished once the next one's
  contents turn up, and linkers do leave the terminator out — `dcs-klone.exe` runs its hunks straight
  into each other.
- `HUNK_OVERLAY`: an overlaid executable continues with an overlay table of hunks loaded on demand;
  the header is as far as the walk usefully goes.
- Trailing data past the last hunk is fine — demos do append their own.

### The generated hard drive

`system/amihdd/` is the skeleton: `C:` with the commands a startup-sequence may reach for (`echo`,
`SetPatch`, …) and `LIBS:` with the system libraries that aren't in ROM. Those shipped on the
Workbench disk, so a drive without a `LIBS:` has none of them. A demo that opens one gets NULL back
and, having nothing to draw with, closes what it did open and exits with a zero return code and no
message — `eph-fels` does exactly that when `lowlevel.library` (keyboard and joypad, which nearly
every AGA demo reads) isn't there, which from outside looks like it never ran. The release's own
copies are copied over these afterwards and win.

`copy_all = !file.is_file() || file.has_meta(RELEASE_DIR)` — the whole release goes into the drive
because the program loads its parts and music off it, *unless* we were handed one loose executable
with no release around it. An override's `boot_file` (or `--boot-file`) leaves the path pointing at
one file inside a release it already unpacked, so `RELEASE_DIR` says so.

**Which executable to start**: a file actually named `*.exe` wins. Failing that, the *shallowest*
executable — the one meant to be run sits at the top of the release while deeper directories hold the
parts it loads (an intro, a trackmo's chapters, a bonus). Ties broken by path so the pick is stable.
Files that claim to be executables but don't parse are kept as a last resort, so a release shipping
only a mangled exe behaves as before.

**Assigns.** `assign` meta is `NAME=path;NAME=path`. The `C:Assign` lines must run before the demo,
so they go at the top of whichever startup-sequence boots it: the generated one in `handle_exe`, or
the release's own via `patch_startup_sequence` — nothing else gets a chance to make them. When
patching the release's own sequence, the drive *is* the release directory, so `C:` holds whatever the
release put there (often nothing, and then every assign line fails silently); the one `Assign`
command is lent from the skeleton drive. The release is copied to a temp dir first because it is
usually read straight off the file system and the mounted drive has to be writeable. Amiga text is
not necessarily UTF-8, so the sequence is handled as bytes. `find_ignoring_case` matches the way
AmigaDOS would — a release ships `C/Assign` or `c/assign` as it pleases.

**`apply_fast`** = accelerated A1200, fast + Zorro III memory, FPU, JIT. What `fast = true` in
`overrides.toml` asks for, for releases where the year-and-tags guessing in `AmigaSystem::load` picks
a machine too small. Applied *before* the rest of an override's meta so an entry can still name a
single option of its own and win.

### `--unadf`

An AmigaDOS demo disk boots much faster as a hard drive than as a floppy the core seeks around. Only
for a **single** disk — a multi-disk release swaps disks by name, which a drive built from one of
them cannot answer. Anything that isn't a mountable, self-booting AmigaDOS disk falls through to the
floppy path.

- `dms_to_adf` unpacks a DMS into a floppy image in a fresh temp dir (the dir owns the image so it
  must outlive it). `None` for an archive that won't come apart is not an error: the cores read
  `.dms` themselves, so the release still boots as a floppy.
- `unpack_boot_disk` returns `None` unless the disk boots *itself* — booting a drive means running
  `s/startup-sequence`, so a disk without one has nothing to start, and a trackloaded disk doesn't
  mount at all. Neither is an error; both are logged at debug. ADFlib reads sector images only, so a
  DMS is turned back into one first and that temp dir must live for the whole walk. The returned
  startup-sequence path is relative, since the caller rebases it onto its own copy.

**puae illegal file names.** `UAE_ILLEGAL_CHARS = % * ? " < > |` — `evilchars` in puae's
`src/fsdb_unix.c`. (`/` and `\` never reach here; the unpacker refuses them outright — `safe_name` in
`src/c_shims/adf_unpack_shim.c`. The rest are ASCII, so looking for them in the UTF-8 host name is the same
as looking for them in the Amiga one.) puae's `get_nname` runs every name a program opens through
`fsdb_name_invalid_dir` and answers "no such file" *before* it looks at the drive, so no host-side
spelling can make the file visible. `3d-demo.adf` boots into nothing that way: its demo loads
`Har vi røget hash?` and the question mark is enough. amiberry has no such check, so a disk like that
is handed to amiberry instead — the core is chosen *after* the unpack for exactly this reason.

## `newsys/disc.rs`

CD image handling shared by the disc-based systems (`playstation`, `neo_geo`): reading ISO9660 out of
a dump in whatever sector layout it arrived in, writing a minimal one from a set of files, picking
apart a cue sheet, and putting a disc into a shape the cores can read. Both users want the same
things for the same reason — a release turns up either as a disc image to identify or as the loose
files that were meant to be burned onto one.

### ISO9660 writing

- `ISO_SECTOR = 2048` (user-data half of a CD-ROM data sector, and the unit every address counts in).
- `LBA_PVD = 16`; the 16 sectors ahead of it are the system area, which the filesystem never uses —
  a pressed disc keeps its console's licence data there (`system_area_has`).
- Sizes and addresses are stored **twice**, little then big endian (`both_endian32`/`16`), so a
  reader of either byte order takes the half it likes. The two path-table pointers are the one pair
  of fields stored as a plain value in each byte order instead.
- Strings are fixed-width and space-padded (`put_padded`).
- `dir_record` takes the identifier exactly as it goes on the disc, so files keep the `;1` version
  suffix and `.`/`..` are the single bytes 0 and 1. Records are padded to even length, which makes
  the one-byte-named entries — and so the copy of the root's record inside the volume descriptor —
  34 bytes. Recording time is a **fixed 1980-01-01 GMT**: nothing on the console side reads it, and a
  constant keeps the image byte-identical between builds, which the content-keyed caches rely on.
- `pack_records`: a record may not straddle a sector boundary, and the zeroed tail of each sector is
  what tells a reader there are no more records in it.
- `build_iso` writes the smallest level-1 image: no subdirectories, no Joliet/Rock Ridge, one path
  table entry — deliberately what a console's own boot ROM was written against. Names must already be
  8.3 upper case (`iso_name` checks). File LBAs are computed in two passes, because where the files
  start depends on how many sectors the root directory takes, which depends only on the name lengths.
  An empty file still gets a sector so its address is one no other extent covers.
- `iso_name`: the length limit is the part that matters and the part enforced. `-` is not strictly a
  d-character but every burner passes it through, and a name here has to keep matching whatever
  already refers to it (a Neo Geo CD's `IPL.TXT`), so mangling would be worse than allowing.
- `IsoSpec::pad_sectors` exists only so a caller can steer the image away from a length some reader
  would mistake for a different sector layout.

### Reading a dump (`DiscImage`)

`SECTOR_LAYOUTS` is `(bytes per sector in the file, offset of the 2048 user bytes)`. A `.iso` holds
user data only. A cue's `.bin` holds whole CD sectors: 12-byte sync + 4-byte address, then — for
Mode 2, which is what PlayStation discs are pressed as — an 8-byte XA subheader, with ECC after the
data. A CloneCD `.img` adds 96 bytes of subchannel on top. **Order matters**: the first layout whose
sector 16 looks like a volume descriptor wins, and Mode 2 is the common case. A `DiscImage` only
exists for a file that really holds ISO9660, since finding the PVD is what identifies the layout.

`root_names` walks the directory by hand rather than using a filesystem crate: every crate on offer
reads a plain 2048-byte-sector image, so a raw MODE2/2352 track would need this sector translation
wrapped around it anyway, and the question here is only which names the root holds. Capped at
`MAX_ROOT_SECTORS = 16` so a corrupt length field can't turn a sniff into a long read.

### Cue sheets

- `parse_cue_files` handles both quoted and bare names (scene sheets use either); the kind is always
  the last token on the line.
- `TRACK_EXTENSIONS = bin, iso, img`.
- `cue_data_tracks` leaves audio tracks out: the question callers ask is which console the disc
  belongs to, and only a data track can answer.
- `cue_is_complete` — a core opens a sheet by opening *all* its tracks, so one name that resolves to
  nothing makes the whole sheet unloadable. Releases do arrive that way (the sheet from the original
  burn, only the data track kept); the bare track beside it is then the better thing to load.
- `cue_for_track` — a sheet describes the whole disc (data track plus CD audio) so it is worth more
  than the bare track. Incomplete sheets are passed over. When a release ships more than one sheet
  for the same track the list is **sorted** so the same one is picked every run, or the disc cache
  fills with copies.
- `resolve_ignoring_case` — cue sheets are written against the burned disc's upper-case names, which
  is rarely how the files were left lying around.

### `prepare_disc` — making a disc the cores can read

No libretro core here decodes MP3 audio tracks; they read the compressed bytes straight through as
PCM, which comes out as full-scale noise. If a cue references any, a parallel disc directory is built
in the cache with those tracks decoded to WAV and the sheet rewritten to match. Data tracks are
**hard linked** (falling back to a copy across filesystems), so the copy is nearly free. The same
rewrite fixes a sheet whose names don't match the files' actual case — what a disc burned from
upper-case names and unpacked onto a case-sensitive filesystem leaves behind. Returns `None` when
every track is already playable.

- `CDDA_RATE = 44100`, 16-bit stereo: the core seeks past the WAV header and reads the rest as raw
  PCM, so the transcode forces mono→stereo and resamples. Both are no-ops for the 44.1 kHz stereo
  MP3s scene discs actually ship, but silently wrong speed or a half-length track is a nasty way to
  find out otherwise.
- Symphonia signals end-of-stream as an IO error; a `DecodeError` (truncated/corrupt frame) is
  skipped rather than losing the track.
- The cache is keyed on the disc's **contents**, never path or mtime: a disc unpacked from a zip
  lands in a fresh temp dir each run and the extractor doesn't restore archived timestamps, so either
  would miss every launch and pile up another copy of the transcoded audio. Size alone would let an
  edited track reuse stale audio, so the *bytes* of the tracks actually decoded are hashed — a few MB,
  unlike the data track, which can be most of a gigabyte.
- The rewritten `disc.cue` is written **last**, so a build that died partway leaves an entry the
  cache knows to rebuild rather than one naming tracks it never wrote.
- The `cdda` cache is the largest demarc keeps (a full disc's audio as WAV), but its accounted size
  *overstates* the disk it costs because data tracks are hard links — so the budget is deliberately
  generous.
- `write_wav` casts `&[i16]` to `&[u8]` unsafely; sound because `i16` has no padding or invalid bit
  patterns and `u8`'s alignment is weaker.

## `newsys/dos.rs`

PC/DOS through **PCem** or **DOSBox Pure**, picked by what the release is:

- A PCem machine `.cfg` — the same file desktop PCem writes into `configs/` and takes with
  `--config` — goes to PCem. It names the machine, CPU, video and sound cards and the disc images to
  mount, so it *is* the whole configuration; the core has no machine picker.
- A bare DOS program (`.exe`, `.com`, `.bat`) goes to DOSBox Pure, which brings its own DOS and
  mounts the directory the program sits in as `C:`. That is what most DOS releases arrive as.

Neither core ships BIOS ROMs — DOSBox needs none, PCem's are copyrighted and must be placed under
`<system dir>/pcem/roms/<machine>/` (`docs/roms.txt` in the PCem tree lists what each machine needs).
Everything the machine writes (NVR, logs) goes under `<save dir>/pcem/`.

`is_pcem_config`: `.cfg` is far too generic to accept on its own, so require the one key every PCem
machine config has and nothing else uses — a `model =` naming the machine.

### Executable sniffing (`exe_kind`, shared with `newsys/windows.rs`)

`ExeKind::{None, Dos, Windows, Legacy}`. The same extension and `MZ` header belong to every Windows
program ever built, and DOSBox can run none of them; what sits at `e_lfanew` tells them apart — a
second header there means the `MZ` is only a stub in front of the real image. `LE`/`LX` is a DOS
extender (how half the demos of the era were built) and counts as `Dos`; `PE` is `Windows`; `NE` is
Windows 3.x/OS2 (`Legacy`, not what any of this is for).

**The `e_lfanew` offset is not required to clear the DOS header.** A size-optimised release — most of
the 64K Windows intros — overlaps the two so the `PE` lands as early as `0x0c` and the fields behind
it double as the rest of the DOS header. Only an offset landing on the `MZ` magic itself is out of
bounds. Plain DOS executables leave the field alone, so anything that isn't a sane offset into the
file is one of those.

`MAX_COM_SIZE = 0xff00` — DOS loads a `.com` into a single segment, below the stack at the top of it.
A `.com` is a raw memory image with no header, so size is all there is to go on. A `.bat` is text and
an empty one starts nothing.

### Picking what to run (`launch_rank`)

A release is usually a directory with one program worth running and several that aren't (an
installer, a setup tool, an .NFO viewer), reached in filesystem order. So:

- The file named after the release is what the release is.
- `INSTALL`/`SETUP` is the one thing we know we don't want.
- A `.bat` loses to any `.exe` beside it: it reads like the author saying "start here", but a release
  shipping one is as often using it to print the .NFO or set a variable before handing over, whereas
  the `.exe` beside it is the demo and starts the same either way.
- An extender/DPMI host (`EXTENDERS`) is loaded by the program linked against it, never started by
  hand — on its own it prints a usage banner and quits — so it takes the same penalty; a release
  shipping one holds the program that needs it too.
- Tie-break: a plain 8.3 name wins (`is_simple_name`). A DOS release could not have been built around
  a name DOS cannot type, so a long or non-ASCII one was given to the file afterwards by whoever
  packed it (an unpacker, a scene archive, a "read me first ⭐.exe" wrapper); the program the release
  actually is still carries the name it was linked as. The bonus is small enough not to reach across
  the ranks above it — an `INSTALL.EXE` stays below a demo whatever the demo is called.

`is_simple_name` uses `to_str` not `to_string_lossy` (a non-UTF-8 name has bytes we'd replace with
U+FFFD and reject anyway), and `rsplit_once('.')` so the *last* dot separates — which also means a
second dot lands in the stem and is rejected there: DOS has exactly one, and `readme.txt.exe` is not
a name it could have held. `DOS_NAME_CHARS` is the alphanumerics plus the punctuation left over once
DOS's own characters (path separators, wildcards, command-line delimiters) are removed.

### DOS/4GW

`dos4gw=true` on an entry (`META_DOS4GW`) copies `DOS4GW.EXE` beside the program. A program linked
against DOS/4GW loads it at startup from the current directory or the PATH, and plenty of releases
were packed without it — it came with the compiler, and every machine of the era had one lying
around. DOSBox starts in the directory it mounted as `C:`, which is where the program sits, so that
is where the copy goes. `DOS4GW.EXE` is uppercased because DOS uppercases every name it prints and
most releases ship it that way. A release that ships its own extender has already answered the
question. Only relevant under DOSBox — a machine config brings its own DOS on its own disc images.

**Aspect**: DOSBox Pure reports the raw framebuffer ratio (a 320x200 mode comes out as 1.6, i.e.
16:10) unless aspect correction is on. With it on, the core leaves the framebuffer alone and reports
the pixel-aspect-corrected display ratio, which is what a CRT showed and what our scaler wants.

Tests read the emulated screen back **as text**: a pixel hash would say the frame changed, not that
the machine booted, and would go stale on any cosmetic change in PCem.

## `newsys/windows.rs`

Windows releases run under wine, inside the gamescope libretro core. Linux only.

`is_windows_program` is the exact complement of the `.exe` half of the DOS check, read from the same
header (`dos::exe_kind`).

### Resolution from the file name (`res_from_name`)

A demo built for one size often says so only in its file name (`demo_1920x1080.exe`,
`elevated_1440_900.exe`). It matters because the size must be settled *before* the demo starts: the
dialog driver picks the mode by matching what demarc asked for against the labels in the setup
dialog, and gamescope is given a session that size.

- `RES_SEPARATORS = [['x','X'], ['_']]`, most telling first. An `x` between two numbers is nearly
  always a size; an `_` is only a separator and could hold apart anything (a year, a version). So
  `elevated_1920x1080` is read by its `x`, and the `_` form is left for `elevated_1920_1080`.
- `MIN_SIDE = 120` … 8K: two numbers with something between them are not only ever a screen mode —
  `pack2x2`, a hex `0x1000` and `demo_2_1` all read the same way to a scan. The bounds are what a
  display could actually be, which throws all three out without understanding the rest of the name.
- Digit runs are taken as they lie, so `vga640x480` reads as well as `demo_640x480` — the name in
  front is no business of ours.

### wine / gamescope wiring

`ARG_SEPARATOR = "\u{1f}"` holds the words of `gamescope_command` apart.

**`capture_meta` restates a Windows entry's settings as the gamescope core's options.** The two name
the same things differently: an entry has always said `wine_res` / `wine_desktop`, and the core —
which also runs Chrome and whatever else a session can hold — says `gamescope_resolution` /
`gamescope_command`. Translating here keeps the entry vocabulary the one people already write and
keeps `overrides.toml` working whichever backend runs the release.

The command is the whole point of doing it here: left alone the core runs `wine <exe>`, which is a
demo sitting on its setup dialog with nobody to answer it. Instead it gets the whole command — the
dialog driver, the resolution to pick, the virtual desktop if asked for — built by
`wine::wine_command`. See `docs/GAMESCOPE.md`. Anything already set explicitly wins, so
`-x gamescope_command=...` overrides the lot, which is how the core is tested against a client that
isn't wine.

- `gamescope_resolution` is the size the driver is about to ask the dialog for, so it is the size the
  session has to be. **Not** read from `wine_res` directly: `pick` is not a size, and the one it
  stands for is the backend's to decide.
- `wine_dll_overrides` → the core's `WINEDLLOVERRIDES`. `d3d*=n,b` — "native for all D3D seems to
  work".
- `wine_gl_compat` is a yes/no an entry's author can answer; what the core exports is the Mesa
  variable itself, so the translation lives here. Only set when the answer is yes — unset is what
  leaves the demo's own profile request alone.
- The prefix is always demarc's own (the one `just wine-prefix` prepares), **never** the user's
  `~/.wine`.
- A release that has gone missing between unpack and start only warns: the core can still make a
  command from the path it is handed, and a demo with an unanswered dialog beats no demo at all.
- A missing `wine` binary is reported once, plainly, at load time — otherwise the session comes up
  empty with only a line in the core's log. Not done for a command someone typed:
  `-x gamescope_command=...` may be anything and often has nothing to do with wine.

### Sandboxing (`sandbox_for`)

Nothing here is fatal: no `bwrap`, a kernel that won't give an unprivileged overlay, or a first run
with no prefix to copy yet all mean the session runs in the shared prefix as it always did — one demo
at a time, which is what demarc did up to now anyway. See `wine_sandbox.rs`.

- Skipped for a hand-typed `gamescope_command`: there is no telling what it is or whether a prefix is
  even involved, and wrapping it in a sandbox it never asked for would make
  `-x gamescope_command=glxgears` harder to reason about.
- A bare file name's parent is the empty path, which is not a directory to start in — left out, the
  sandbox keeps demarc's own working directory, which is what that name was relative to anyway.
- **Unsandboxed only**: `close_prefix()` is called at load. Every unsandboxed session runs in the one
  prefix, so anything still alive in it belongs to a demarc that never got to shut it down (a crash,
  a core unloaded without teardown); clearing it keeps those from piling up one tree per launch. It
  is also exactly why this cannot happen when there *is* a sandbox — the shared prefix would be
  closed out from under a demo using it.
- A sandboxed prefix is nobody's to close but the kernel's, so the core's `StopWineServer` teardown
  is disarmed: `wineserver -k` reaches a socket in the sandbox's own `/tmp`, unreachable from
  outside, and there is nothing to reach anyway — the pid namespace goes when the demo does and takes
  wine's services with it. Left armed, teardown would spend its timeout talking to a prefix that no
  longer exists.

## `newsys/playstation.rs`

**Wrapping a raw PS-X EXE in a disc.** pcsx_rearmed loads discs but refuses a raw PS-X EXE; Beetle
takes the executable directly but needs a real BIOS, which is the thing this avoids having to ask
for. So `create_psx_iso` lays the executable out as a bootable disc: a `SYSTEM.CNF` naming the boot
file plus the executable itself, in a filesystem with nothing else in it.

The console's boot path dictates the shape: it reads the volume descriptor at sector 16, follows the
root directory record inside it, looks for `SYSTEM.CNF`, takes the `BOOT = cdrom:\…` line, and loads
that file as a PS-X EXE — the file's first sector *being* the executable's own 0x800-byte header,
which is why the executable goes on the disc unaltered. Cores that HLE the BIOS (pcsx_rearmed) walk
the same structures themselves. `BOOT` is the only line a core reads; the rest of the CNF is what a
real disc carries. `ISO_EXE_NAME = "PSX.EXE"` is the default both the BIOS and every core fall back
to when a disc has no `SYSTEM.CNF`, so it also makes the image boot if the sheet is ignored.
`system_id = "PLAYSTATION"` costs 11 bytes and some tools identify a disc by it.

**147-sector gotcha**: pcsx_rearmed tells a 2048-byte-sector image from a raw 2352-byte one *by the
file's length alone*, so a length dividing evenly by both is read as the wrong kind of disc — which
happens at a multiple of 147 sectors. An extra empty sector steps past it (`IsoSpec::pad_sectors`).

**`t_size` repair.** `PSX_HEADER_LEN = 0x800`, `t_addr` at `0x18`, `t_size` at `PSX_TEXT_SIZE_OFFSET
= 0x1c`, `PSX_RAM_SIZE = 0x20_0000`. The core takes only one number: `t_size` must equal the data
following the header byte for byte, or it refuses to load ("Text section recorded size is
smaller/larger than data available in file"). Scene releases keep hitting the small side — the demo's
data was appended after the code without updating the header — so record the size the file really
has. But the section must not run off the end of RAM: RAM is mirrored the way the hardware maps it,
so the overflow lands back at address zero on top of the kernel and the demo dies. Only the load
address' offset within RAM says how much room there is. A file with nothing after its header is
broken in a way no size helps with; leave it to fail with the core's own complaint. The repair is
applied to *our copy* — the file we were handed isn't ours to write to.

**Identification.** `PSX_LICENCE = "Sony Computer Entertainment"` lives in the system area (the 16
sectors ahead of the filesystem) and needs no filesystem — but scene images often have it stripped
(a rip of just the data track tends to zero it), so its absence proves nothing. What every bootable
disc has is `SYSTEM.CNF` in the root, or `PSX.EXE` when the boot file kept its default name — and
that is also what separates a PlayStation disc from a Saturn or PC Engine one arriving in the same
MODE2/2352 wrapper. A cue sheet missing any of its files is no use to the core whatever it describes,
so it doesn't count — leaving the walk to reach the bare data track beside it.

**Preference order**: cue > disc image > wrapped executable. A cue describes the whole disc (data
track plus CD audio) so it wins over the track it names; a release shipping both a disc and an exe
has the disc as the real thing. The audio encoding is not settled here — `NewSys::load_file` puts any
cue a system picked through `disc::prepare_disc` before the core sees it. A broken executable
shouldn't take the whole release down; there may still be a disc image next to it.

Built images are cached under the executable's **contents** (`psxexe` cache), because the file is
often unpacked to a fresh temp dir every launch. Small entries, so the budget only has to keep a long
browse through a pile of `.psx` files from accumulating without limit.

## `newsys/neo_geo.rs`

`IPL_NAME = "IPL.TXT"` is the boot list at the root of every Neo Geo CD disc: one
`NAME.EXT,<bank>,<offset>` line per file, which the BIOS reads and loads into the memory area the
extension picks (`.PRG` → work RAM, `.FIX` → fix layer, `.SPR` → sprite RAM, `.Z80` → sound CPU,
`.PCM` → ADPCM). Its presence in the root is what makes a disc — or a directory — a Neo Geo CD one.

**`create_neocd_disc`** burns a directory of loose files onto an image plus a cue naming a single
MODE1/2048 data track (the sector size the ISO is written in, so the core reads it straight through
with no sector translation) — which is what geolith wants. Releases ship this way because that is
what the devkit produced, and nothing in the chain takes them as they are: the core only opens a
disc, the BIOS only reads a filesystem. Only files directly in that directory go on: a release keeps
sources and readmes in subdirectories, and the boot ROM reads a flat root anyway. Entries are sorted
by on-disc identifier — ISO9660 wants the root in that order, and it also makes the image (and the
cache key) the same whatever order the filesystem hands entries back in. A name that isn't 8.3 is
left off: the BIOS reads a level-1 filesystem and could never open it. A file the boot list names but
the disc doesn't carry is a hang on the loading screen with nothing to explain it, so it is reported
up front. `COPYRIGHT_FILE = "CPY.TXT"` and friends: nothing in the boot path reads the volume
descriptor's copyright/abstract/bibliographic fields, but a real disc has them and they cost three
directory entries already on the disc.

The `neocd` cache is keyed on directory **contents**, never path (a release unpacked from an archive
lands in a fresh temp dir every launch). `disc.cue` is written **last**, so its presence tells
`FileCache` the entry finished.

**`.neo`** is ambiguous: `NEO_ROM_MAGIC = b"NEO"` (the NeoSD container tag, ahead of a version byte
not worth being fussy about) has to be checked, because `.neo` is also what NEOchrome saves an Atari
ST picture as (see `degas.rs`).

A CHD is compressed so there is no cheap way to look inside for the boot list; nothing else here
reads one, so take it. A release often points at one file inside the disc directory (the `.prg` the
boot list loads first) rather than the directory itself — the disc is the whole directory it sits in.

**Preference order**: cartridge ROM first (needs no BIOS disc and boots instantly) > cue > CHD >
a disc that exists > one that would have to be built. A disc boots through the CD BIOS the same way
real hardware does and geolith gives up with only its own message if it isn't there, so a missing
BIOS file is named while there is still context to name it in.

## `newsys/atari_st.rs`

**`MACHINE_TYPES = ["st", "ste", "tt", "falcon"]`, first is hatari's default.** hatari is handed
`hatari_machinetype` as it stands and rejects an unknown value by **abandoning the rest of its
command line along with it** — including the `--harddrive` that mounts C:, which leaves a hard drive
release sitting on the TOS desktop with no drive to run from. So nothing may reach it but the four it
knows. `date` is demarc's own stand-in for "decide from the release's year" and is never a machine
the core knows, so it must be resolved whether the year says anything or not; undated defaults to
`st`, because an STE release usually says so while plenty of ST ones break on an STE.

**Falcon/TT hires.** Hatari sizes its internal "desktop" from the libretro core's `retrow`/`retroh`,
which only the ST/STE renderer ever updates — the Falcon/TT Videl path never does. Left at the core's
low-res default (392x248) every Videl mode is larger than that fake desktop, so `hostscreen.c` halves
it ("too large screen size 640x480 -> divided by 2x2") and draws the shrunken image into the top-left
of the frame. Setting `hatari_video_hires` raises it to 832x548, which covers the usual Falcon modes.

A release shipped to run off a hard drive is a late one: it expects the STE and the memory that came
with it, and a demo like molz quietly drops back to the desktop on the 1 MB ST that is the core's
default (`docs/NOTES.md`). So a hard drive load upgrades a *guess* — but never a request the release
itself made.

**GEMDOS drive (`build_gemdos_drive`).** hatari mounts a host directory as drive C: and boots from
it; the libretro core takes the drive as a `.gem` file whose name minus the extension is the
directory to mount, so the two are created side by side with the `.gem` itself empty. Booting runs
`C:\AUTO\*.PRG`, so that is where the program goes, renamed to `AUTO_PROGRAM = "STARTME.PRG"` — TOS
only auto-starts `.PRG` from there, so a `.TOS`/`.TTP` demo has to be renamed, and 8.3 keeps GEMDOS
from mangling it. A release that already keeps its program in `AUTO` starts itself and is left alone;
any *other* `AUTO` folder is moved to `NOAUTO`(2,3,…), because what a hard drive release carries
there is usually the disk-swap stubs of its floppy version, which stop the boot dead ("insert disk 1
and reboot"). Everything is copied into a temp dir rather than mounted in place: the drive is
writable from the emulator and the `AUTO` folder is ours to rearrange — neither is something to do to
a directory of the user's own. The release is `base` and nothing outside it: pointed straight at a
program file, the release *is* that one file, because the directory it happens to sit in belongs to
whoever pointed us there (a downloads folder, a home directory) and copying that wholesale is how the
drive grows to gigabytes and the copy never finishes.

**`pick_program`.** `boot_file` decides outright, matched by file name or by path within the release
(`DEMO/TLKTLK2.PRG`), case-insensitively, with `\` normalised to `/` so a `boot_file` copied out of a
release's own README still matches. Otherwise, in order: named like a program
(`PROGRAM_EXTENSIONS = prg, tos, ttp, app`), nearest the top of the release, then the biggest of
those — with the path as a final tie-break so the order doesn't change from one walk to the next.
Size alone picked a readme viewer over the demo beside it, and picked a payload over the loader that
feeds it: `molz.tos` is a 651-byte loader next to the 652K part it loads (`docs/NOTES.md`). The sort
terms are written so `false` (the wanted answer) sorts first.

## `newsys/c64.rs`, `newsys/plus4.rs`

**`is_c64_prg`**: a PRG is a 2-byte little-endian load address followed by the data to place there,
so it can never reach past the top of the C64's 64K address space. VICE rejects anything that does,
and other systems use the same extension (Neo Geo for one), so check the range rather than trust the
name. Needs the load address plus at least one byte of data.

Note in the code: after `make_temp()`, if the incoming release was a single file the path switches to
the parent dir.

**plus/4** uses `yapesdl`, built as a libretro core from `external/yapesdl` (`Makefile.libretro`).
VICE's plus/4 emulation is incomplete enough that releases ship warning about it, which is why this
core exists here. `yape_model = "Commodore Plus/4 (accurate)"` — the cycle-exact TED is the whole
reason for preferring the core; the "fast" model is for machines that can't keep up.

Nothing in a `.prg` or `.d64` says which Commodore it was written for — a plus/4 program and a C64
one are the same shape — so the machine cannot be detected and must be asked for with
`--cbm-variant c16`. Without it `Plus4System` stands aside and `C64System` takes the release. The
core loads `prg`/`p00`/`t64` straight into memory, so none needs converting the way the C64 side
does. Several disks become an m3u the core reads as its disk list. Nothing marks the main program
among a release's extras, so programs are sorted for a stable pick.

## `newsys/adf.rs` — ADFlib

Unpacking an AmigaDOS floppy image into a real directory, backing `--unadf`. Only works for a disk
with a real file system; plenty of demo disks are trackloaded (custom boot block, raw sectors, no
AmigaDOS structures) and mount as nothing. **Being handed one of those is an expected outcome, not a
fault** — it comes back as an `Err` the caller logs and shrugs off, and the disk is booted as a
floppy.

The walk is C (`external/ADFlib` via `src/c_shims/adf_unpack_shim.c`, where the reasoning for that lives).
**Not** the `adflib` crate on crates.io, which was tried first and cannot do this: as of 0.1.7 it
scans only longwords 24..51 of a directory's 72-entry hash table and never follows the `next_hash`
chains (silently missing most files); it reads an entry's size out of the `header_key` field, so
reported sizes are block numbers; and `FileInfo` carries no block pointer while `extract_file`
refuses directories and only searches the root, so there is no way to descend into `s/` at all. On
`Ghostown-SushiBoyzParty.adf` it finds 2 of 6 root entries, reports `S/` as a file, and never sees
the demo executable. This code's output is byte-for-byte identical to ADFlib's own `unadf` on that
disk — the one divergence is a non-ASCII file name, which `unadf` writes as the Latin-1 the disk
holds and this writes as the UTF-8 the cores read (`safe_name` in the shim).

Errors: `ADF_ERR_OPEN -1` (not openable), `ADF_ERR_MOUNT -2` (no AmigaDOS volume — a trackloaded
disk, most likely), `ADF_ERR_IO -3` (host-side copy failed).

**Threading**: ADFlib keeps its environment (log callbacks, device driver list, dir-cache flag) in
globals, so only one unpack may be in flight — hence the `ADFLIB` mutex. Releases load on a worker
thread and `--grid` loads several at once, so this is a real race, not a theoretical one.
`adfAddDeviceDriver` appends to a global list with no duplicate check, so init must happen exactly
once (`INIT: Once`) or the list grows without bound. A poisoned mutex is carried through: ADFlib's
globals are re-initialised per call apart from the driver list.

## `newsys/dms.rs` — xDMS

DMS (Disk Masher System) is how much of the Amiga scene was distributed: a whole floppy, track by
track, in one compressed file. **Both Amiga cores read `.dms` directly**, so nothing here is needed
to *boot* one — it exists for `--unadf`, whose ADFlib walk only understands a plain sector image.

xDMS 1.3 by Andre Rodrigues de la Rocha (public domain), taken from amiberry's copy, in
`external/dms`; entry point `src/c_shims/dms_unpack_shim.c`. It writes each track at its own offset in the
output, so a truncated or partly corrupt archive still yields an image with whatever tracks it did
contain **in the right places** — including, usefully, the boot block and root directory.

**xDMS is a single-threaded program that happens to have a function in it**: `dms_text` (the LZ
window), the bit buffer in `getbits.c` and every decruncher's tables are file-scope globals. Running
two at once corrupts the heap — hence the `XDMS` mutex (again a real race under `--grid`).

Errors: `-1` open, `-2` write, `-3` not DMS (or an FMS archive, which holds files rather than a
disk), `-4` wouldn't come apart (bad CRC, unknown compression mode, or a password — the one we can do
nothing about), `-5` unpacked to nothing. A successful unpack returns 901,120 bytes for an ordinary
DD disk, twice that for HD, less when tracks were missing.

## `newsys/atari_xl.rs`

`HEADER_LEN = 6`. An Atari 8-bit executable is the DOS binary load format under whatever extension
the release felt like (`.xex`, `.com`, `.exe`, `.bin`, none), so it is recognised by content: `$FFFF`
followed by the first segment's start and end address, little-endian, start not past end. (`$FFFF`
may repeat before a segment but never as the address itself.) The core detects it the same way and
loads it without DOS, so the file is handed over as-is.

`.atr` disk images are recognised by the magic `$0296` ("NICKATARI") plus a legal sector size —
**the extension is not ours alone**: the ZX Spectrum names its 768-byte attribute dumps `.atr` too,
so taking every `.atr` would claim Spectrum pictures away from the image system and boot a colour map
as a disk.

Extra binaries are sorted for a stable pick, since nothing distinguishes the main program.

## `newsys/snes.rs`

`COPIER_HEADER_LEN = 0x200` — the header a ROM copier (a Super Wild Card and clones) writes in front
of a dump. Emulators skip it, but a scene release whose cartridge header was blanked may have nothing
else left to identify it by. Signature at offset 8, machine in the third byte: `0x04` SNES, `0x06`
Megadrive.

`SNES_HEADER_OFFSETS = [0x7fc0, 0xffc0, 0x40_ffc0]` — last page of the first bank (LoROM), of the
second bank (HiROM), and 4 MB in (ExHiROM). `SNES_BANK_SIZE = 0x8000`; a ROM is always a whole number
of banks. `MAX_ROM_SIZE = 16 MB` (no cartridge ever shipped with more than 8 MB, ExHiROM included).

In the 64-byte header **everything is advisory** except two things: scene releases routinely leave
the title blank, the map mode zero and the ROM size field describing some other cart. What must hold
is the checksum at `0x1e` and its complement at `0x1c` summing to `0xffff` (checked with xor, which
sidesteps the carry — and a checksum of zero passes that test but describes an empty ROM, so it is
ruled out), and the emulation-mode reset vector at `0x3c` pointing at the ROM half of a bank. A ROM
with a zeroed header fails this and is caught by the copier header instead — **both paths are
needed**, since plenty of ROMs ship without a copier header at all.

## `newsys/gba.rs`

`GBA_HEADER_LEN = 0xc0` — the whole cartridge header, up to but not including the entry point. A GBA
ROM opens with an unconditional ARM branch past the header (`b` at offset 0, 24-bit signed word
offset measured by the ARM pipeline from `0x08`; the entry must land past the header), then the
156-byte Nintendo logo at `0x04` and a fixed `0x96` at `0xb2`. Only the logo's first 8 bytes are
compared — a ROM that got that far is never anything else.

**Scene releases meant for a flash cart or emulator often blank the logo** (it is Nintendo's artwork
and only the real BIOS cares), so a ROM without it still counts if the rest holds: reserved fields
zero (`0xb3` main unit code and `0xb4` device type are 0 on everything but Nintendo's own debug
hardware; `0xb5..=0xbb` and `0xbe..=0xbf` are reserved) and the complement check over `0xa0..=0xbc`
correct. That checksum is computed over the bytes right before it, so hitting it by accident takes
the same 1-in-256 luck as each fixed byte on top of it.

## `newsys/pico8.rs`

`CART_PNG_SIZE = (160, 205)` — a PICO-8 label cart is a PNG of one fixed size (the 128x128 screen
inside its border) carrying the cart in the low bits of the pixels. fake-08 rejects any other size
outright, so testing the size here is the same test the core applies. `PNG_HEADER_LEN = 24` (PNG
signature 8 + IHDR length and type 8 + width and height 8).

The extension can't decide: carts are distributed as `.p8.png` but every screenshot beside them in a
release is a `.png` too, and those belong to `ImageSystem`. The IHDR dimensions tell them apart.
`.p8` is PICO-8's own extension and nothing else wants it, so it stands on its own.

## `newsys/images.rs`

Two tiers. `INDEXED_EXTENSIONS` (16) are formats carrying their own palette and with it any colour
cycling — an ILBM or DEGAS picture **is the release**, not a screenshot of one.
`SCREENSHOT_EXTENSIONS` (`png bmp jpg jpeg gif tif tiff tga`) are truecolour and in a release
directory are almost always a screenshot of the real thing. Ranked first by tier, then by path, so a
picture that brings its own palette wins over a screenshot in the same directory; the sort is stable
so equal-rank files keep walk order.

Content is sniffed as well as matched by extension: an ST picture turns up named after the demo it
came from as often as `.pi1`. A Spectrum screen has no header at all, so its **size** is the only
check there is — and it is what keeps an unrelated `.scr` (a script, a screensaver) out of the
running. `HEADER_LEN = degas::SNIFF_BYTES`, the longer of the two checks.

## `newsys/music.rs`

Bare music files (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC, AHX, TFMX, …) played by `MusicEmu` rather
than a libretro core. Extensions are allowlisted "to avoid crashes".

**`vis_script`** picks the Luau visualizer: `--lua` wins outright and is taken as given rather than
probed for — someone who named a script wants to hear about a typo in it (as a load error from the
visualizer) rather than silently get the default. Then a copy in the user's config directory, so a
visualization can be worked on without touching installed files; otherwise the one in `system/`.
`build.rs` packs that into the embedded `system.zip` so the default is always there, and debug builds
read the repo's `system/` in place, which is what makes editing it worthwhile.

## `newsys/web.rs`

HTML/JS releases shown in an undecorated Chrome inside a gamescope session — a picture source like
any other, so the page gets the shaders, grid and screenshots every other system gets. Same core as
the wine side (`newsys/windows.rs`); see `docs/GAMESCOPE.md`. Linux only (`CAN_RUN_WEB`), and the
core is not on the libretro buildbot so it only ever resolves through `DEMARC_CORE_DIR`.

**Only the page is claimed, not the files around it.** A release ships its `.js`, textures and
shaders beside the `.html` and Chrome fetches those itself over `file://`; they are not separately
loadable and must not be taken away from the image and music systems, which can at least show what a
release shipped. A page is 800x600 by default like everything else — unlike a demo it has no opinion
of its own, since nothing in an `.html` announces the size it wants, so the entry has to say.

## `retro_emu.rs` (`RetroCoreDirect`)

The raw FFI / environment-callback side of a libretro core.

**Core duping.** `dlopen` returns the same mapping (and the same C globals) for a core loaded twice
from the same path, so two instances of one core would stomp each other's global state and crash.
The core is copied to a uniquely-named file in a private temp dir first — the trick libretro
frontends use for "core duping" — so every instance gets its own mapping with independent globals.
The temp dir is held in the struct (`_core_tempdir`) and removed on drop.
`RETRO_ENVIRONMENT_GET_LIBRETRO_PATH` still reports the core **as it lives on disk**, not the private
copy: that is how a core finds what was unpacked beside it, and nothing was unpacked beside the copy
(the gamescope core looks there for its compositor).

**`CURRENT_EMU` / `CurrentEmuGuard`.** The C callbacks get no user-data pointer, so a thread-local
points at the instance for the duration of a call into the core. **Every entry point needs one** —
cores call the environment callback from `retro_reset` and `retro_deinit` as readily as from
`retro_run`, and servicing those with a null `CURRENT_EMU` leaves the core holding an unfilled
out-parameter (dereferencing, say, the system directory it asked for during reset). The guard
restores the previous value rather than clearing, so nested entry points compose.

**Shutdown order: `retro_unload_game` then `retro_deinit`, then `dlclose`.** The unload matters most:
a core that runs its emulation on a thread of its own only stops that thread there. DOSBox Pure is
one — its `retro_deinit` frees a couple of buffers and nothing else, so skipping the unload leaves
the DOS thread running and the `dlclose` then pulls the code out from under it: a SIGSEGV in a thread
with no Rust frames in it at all. It is called on the thread that called `retro_run`, which is what
cores that hand work to another thread expect — the shutdown handshake is with the frontend thread
they have been synchronising with. `unload()` is idempotent (`lib` is taken, `Drop` checks it).

**Options.** Our options go in *before* the core is told anything, so they are already there whenever
it announces its own defaults (usually from within `retro_set_environment`, but atari800 and friends
do it later) and whenever it reads them back. `SET_VARIABLES` format is
`"Description; default|opt2|opt3|…"`, and only fills gaps: a value we were given outranks the core's
default. Option `CString`s live in a static map and are never mutated after `SET_VARIABLES`.

**VFS** is answered (see below) — not a nicety: modern Stella refuses to load *any* ROM without one.
The frontend reports back the version it actually implements, which may be newer than what was asked.

**Controller ports.** Both ports are declared joypads at startup: several cores (VICE among them)
leave a port silent until the frontend selects a device for it, so without this `set_joypad` state is
never polled. The mouse is only wired to port 0.

**Loading content.** For a directory (puae mounts one as a virtual hard drive) and for any
`need_fullpath` content, pass `data = null, size = 0` and let the core use the path — there are no
bytes to hand over, and reading a directory errors with `IsADirectory`. The path is **canonicalized
to absolute**: cores like puae resolve m3u playlist entries relative to the playlist file's own
directory, so a bare relative filename leaves them with no base dir and they insert zero disks. On
Windows, `canonicalize()` adds the `\\?\` extended-length prefix which most C libraries (libretro
cores included) don't understand — `utils::strip_verbatim_prefix` removes it.

**Do not poll `retro_get_system_av_info()` per frame.** `av_info` is captured once after
`load_game` and kept current by the `SET_SYSTEM_AV_INFO` / `SET_GEOMETRY` callbacks. Some cores
(atari800) re-run `update_variables()` inside `get_system_av_info`, so polling it every frame
triggers a costly texture/option reinit every frame.

Per libretro, a non-positive `aspect_ratio` in the geometry means "use `base_width / base_height`".
Mouse deltas accumulate as `i32` to avoid overflow, clamp to `i16` when the core polls, and reset
after each `retro_run`. `frame_serial` is bumped by every `run`, since each leaves a freshly rendered
frame.

## `retro_emu/threaded.rs` (`RetroCoreThreaded`)

Runs a core on its own thread so the frontend never blocks on emulation; commands out over a channel,
finished frames back.

**`WORKER_STACK_SIZE = 32 MiB`**, well above the 2 MiB default. Cores recurse deeply on this thread —
a dynarec or shader compiler can overflow the default and take the process down with a SIGSEGV that
looks nothing like a stack overflow.

**`STATE_SKIPPING` is set on the main thread** the moment a skip is requested, not by the worker when
it picks the command up: the frontend polls state every displayed frame and would otherwise see the
skip as already finished during the frame or two the command spends in the channel. The worker only
ever *clears* it, on the frame the skip runs out, so the two sides never fight over the bit. A
zero-frame skip is a no-op, so it explicitly clears the bit the sender optimistically set.

**`SILENCE_SUM = 1000`** — `audio_sum` adds the absolute value of every sample in the frame, so over
the ~1700 samples a frame carries (44.1 kHz at 50 Hz) this is an average magnitude below one unit out
of 32767: digital silence, with room for a core that idles on a small DC offset rather than exact
zeroes.

**`KEY_HOLD_FRAMES = 2`** — long enough for any core to notice a press. Scheduled keys are stored as
absolute frame counts computed from the worker's current frame, so nothing can be scheduled into the
past (a core's startup keys can't land behind it).

`update_rx` is wrapped in a `Mutex` purely so the type is `Sync` (Bevy requires it); `mpsc::Receiver`
is `Send` but not `Sync`. All access is `&mut self` via `get_mut`, so the lock is never contended.

**Benchmark mode** never blocks on the consumer: hand off the latest frame if there is room,
otherwise drop it and keep emulating flat out, so throughput reflects the core rather than the
vsync-limited main loop.

**`SHUTDOWN_TIMEOUT = 5 s`**, deliberately generous: it covers a core still finishing the frame it is
in, and every core here is done long inside it. What it rules out is a core wedged in its own
shutdown taking the whole application down, since this runs on the main thread while the user is
trying to quit. On shutdown the update channel keeps being drained: the worker only checks for
`Unload` at the top of its loop and may be parked in a full bounded `update_tx.send()`.

## `retro_emu/vfs.rs`

`RETRO_ENVIRONMENT_GET_VFS_INTERFACE`. Cores are supposed to ask the frontend to touch files rather
than calling `fopen` — that is how RetroArch supports Android SAF URIs, network shares and paths
inside archives. For demarc every path *is* an ordinary filesystem path, so this is a thin wrapper
over `std::fs`.

**It is not optional any more.** Upstream Stella (`stella-emu/stella` 51994c0, 2026-05-17, "libretro:
Make FSNodeLIBRETRO a proper FSNode implementation") made `FSNode::isFile()` default to `false` and
set it *only* from the VFS `stat()`. With no VFS on offer, `libretro_vfs` stays null, the flag stays
false, and `OSystem::openROM` throws "Unrecognized ROM file type" for every ROM — the check runs
before anything looks at the image, so nothing loads at all. Worse, without a VFS Stella falls back
to the in-memory image padded to `Cartridge::maxSize()` and hashes 512K of mostly zeroes: a 32K demo
came out as MD5 `16cf3ddf…` "4K* (512K)" instead of `9c0e06f1…` "F4* (32K)" — wrong bankswitch type,
wrong per-ROM properties, wrong TV format. Answering fixes the load *and* the detection. Expect more
cores to follow.

`VERSION = 3` (the version that added `stat`/`mkdir` and the directory calls); `stat_64` from v4 is
not in our bindings and no core we load needs it. A core asking for more is refused, as the API
requires.

**The interface is leaked deliberately**: the API says it is owned by the frontend and must outlive
every core — one 152-byte table of function pointers for the life of the process, shared by all
cores, with no per-core state to tear down.

Handles are `Box::into_raw`'d Rust structs handed back as the opaque `retro_vfs_*_handle`. A core
owns each handle between `open` and `close` and never shares one across threads, so the `&mut` taken
on every call cannot alias.

### `seek` returns 0 on success — `fseek` semantics, NOT the new position

The header says otherwise: `retro_vfs_seek_t` is documented as "The new position, or -1 if there was
an error". **Nothing implements that.** libretro-common's reference `retro_vfs_file_seek_impl` ends
in a plain `fseeko(...)` and so returns 0/-1, which is what RetroArch hands cores, and cores are
written against it: a core including `file_stream_transforms.h` gets `#define fseek rfseek`, so its
ordinary `if (fseek(f, off, SEEK_SET)) return -1;` treats any non-zero return as failure. Returning
the position looks fine until an offset is non-zero — pcsx_rearmed reading sector 0 works, sector 4
seeks to 9408, the core reads that as an error ("cdrom read failed for lba 4: -1") and the whole disc
is rejected as an "unsupported/invalid CD image". **Every CD-based system in demarc failed this way.**

### Other VFS details

- `to_path`: on Unix the bytes *are* the path — no UTF-8 round trip, so a filename that isn't valid
  UTF-8 (which demo archives do produce) still opens. Elsewhere UTF-8 is required, which is what
  those platforms hand us anyway.
- `get_path` must hand back exactly the string we were given, so `FileHandle` stores the original
  `CString` rather than re-encoding the `PathBuf`.
- `RETRO_VFS_FILE_ACCESS_UPDATE_EXISTING`: "opens a file without discarding its existing contents,
  only meaningful with WRITE". Without it a write mode truncates. Update-in-place requires the file
  to exist and is the mode used to patch a save file. Neither READ nor WRITE is an error — the API
  requires at least one.
- `open()` must fail on a directory (`opendir` is the call for those): without the check an
  `O_RDONLY` open of a directory succeeds on Linux and only fails later, at the first read.
- A short read is not an error: the caller gets the count and asks again, which is how
  `filestream_read` behaves.
- `stat` uses `metadata`, **not** `symlink_metadata` — a symlink to a ROM should stat as the ROM. The
  size out-parameter is `i32`, so anything past 2 GB saturates rather than wraps: a caller that
  size-checks against a cartridge limit then rejects the file instead of seeing a negative or
  absurdly small one. Nothing demarc loads is that big.
- `mkdir` checks `is_dir` first because `create_dir_all` is happy with an existing directory but the
  API distinguishes the two cases and callers branch on `-2`; losing a race to another creator is
  still "already exists".
- `DirHandle` reads the listing in full at `opendir`. The API only promises a name stays valid until
  the next `readdir`, but holding the whole listing is simpler and keeps names alive for the life of
  the handle, which is stricter. `.`/`..` are never listed (`read_dir` omits them, and handing them
  back only invites a caller to recurse into itself). `file_type()` doesn't follow symlinks, so they
  are resolved to match what `stat` reports. A name with an interior NUL is skipped — it can't be
  expressed in this API and can't name a real file anyway. After the last entry `pos` parks past the
  end so repeated calls keep returning false rather than wrapping to the first entry.

## `jobs.rs`

Background jobs: long, blocking work (downloading a release, unpacking an archive) run off the main
thread and polled from an ordinary Bevy system.

### Why there is no async runtime here — read before adding one

Every I/O crate demarc uses is blocking (`ureq`, `suppaftp`, `zip`, `unarc-rs`, `fatfs`), so there is
nothing to `.await`; adding tokio or smol would only mean calling the same blocking code through a
runtime. What this module borrows from `async` is just its *handle* type: `bevy::tasks::Task` is a
future-like object that can be polled without blocking, and Bevy's `IoTaskPool` is a thread pool that
exists precisely to run blocking I/O. So a job body is a plain blocking closure wrapped in an
`async move` block that never awaits anything. No new dependency, no runtime, no colouring the rest
of the app async.

### API

`Jobs<T>` is one resource per result type; `JobsPlugin` registers `Jobs<PathBuf>` (which covers
`download` and `unpack`), other types need an `AppJobsExt::add_job_type` call. Completion arrives as
a `JobFinished<T>` message in `Update`. `Job<T>` is the handle form — hold it in a component and
`poll()` each frame — used where the result belongs to one specific owner (a per-entity download),
where routing through a global message would only mean filtering by id again.

The module is `#![allow(dead_code)]`: it is a general-purpose facility whose API is deliberately
wider than its current callers, which use only a corner of it.

- **Cancellation is advisory.** Dropping a `Job` abandons it: the blocking body still runs to
  completion on its pool thread (nothing can interrupt it) and its result is discarded.
  `JobError::Cancelled` is reported whenever the flag was set by the time the body returned, even if
  the body ignored it and ran to completion.
- `Job::taken` exists because polling an `async_task::Task` after it has completed **panics**; the
  flag is what makes a repeated `poll` return `None`.
- `JobFinished::result()`/`take()` rather than a public field, so a non-`Clone` `T` can be moved out
  via `MessageMutator`. Taking hides it from every other reader, so only take when this system owns
  the job.
- `Jobs::cancel` takes `&self` (the flag is atomic), so a system holding `Res<Jobs<T>>` can cancel
  without triggering change detection.
- `poll_jobs` reads through `Deref` first so that with nothing in flight `Jobs<T>` isn't marked
  changed every frame.
- `Jobs::default` is written by hand: `derive(Default)` would demand `T: Default`, which job results
  need not be.
- `JobProgress`: `total == 0` means *unknown*, which `fraction()` reports as `None` — an FTP server
  that won't answer `SIZE` leaves a download in that state, so a progress bar needs an indeterminate
  mode. `download_and_unpack` flips back to `set_total(0)` for the unpack phase so the bar doesn't
  sit at 100% for the rest of the job.
- `Jobs::unpack` fails when the file isn't a recognised archive (where `utils::unpack_into` returns
  `Ok(false)`) — a job that silently did nothing is harder to act on than an error.
- `download_and_unpack`'s temp dir outlives the job, so the caller owns it and must remove it.
  Non-archive downloads are left alone and simply copied in.
- A URL already in the cache still round-trips through the pool; it just finishes a frame or two
  later.

## `emu_file.rs`

**`DOWNLOADS_IN_PROGRESS`** is a global atomic rather than per-`Emulator`, because the UI draws one
indicator for the whole window and has no emulator to ask. Kept in step by
`download_started`/`download_finished` around the job in `Emulator::load_async`.
`download_finished` saturates at zero so a stray extra call can't wrap the counter into a permanent
"downloading" state.

**Everything is `&'static str`.** The file list is built once at startup and kept for the whole run:
the db is read into a leaked text the fields are sliced out of, and the few strings built at runtime
(m3u tags, file stems) are leaked one by one. Entries are therefore cheap to clone and hand around,
with no per-entry `String` allocations.

**`UrlList`** keeps the db's own text rather than parsed `Url`s. A db is mostly URLs — one line often
carries several — and a parsed `Url` costs a `String` plus byte offsets into it, where the text is
already `'static`. URLs are parsed once on the way in, to warn about and drop anything that isn't
one, and the text of the survivors is kept. An empty list means the entry has nothing to fetch and is
dropped by the caller.

**`cache_keys` / `cache_key` — do not "simplify".** The download cache keys entries on the
*normalized* URL (what `Url` prints back), not the db's own text. The two differ wherever parsing
rewrites a URL (a bare host gaining its trailing slash, an escape being canonicalised), and feeding
the raw text to `fetch` would orphan every entry downloaded up to now and quietly re-fetch it.
Normalizing here keeps the existing cache matching. (The comment on
`fetch::fetch_url_with_progress` describes the key as the URL as the db writes it, which is what the
raw text would give — so this is the thing to drop if the cache is ever allowed to churn once.) A URL
that doesn't parse is passed through untouched; `UrlList` drops those on the way in, so this only
stands in for lists built by hand.

**`release_downloads`** — a `download` field mixes three things: the release itself, extras (music
rips, scans) and *alternative copies* of the release (a mirror, a reupload, the same demo packed as
`.adf` and as `.dms`). Alternatives and the disks of a set look alike and want opposite handling, so
they are told apart by `disk_stem`: same stem, same disk.

- If any URL is a disk image the release is disk-based and those images make up the first attempt,
  one disk per distinct stem. Everything that isn't a disk image then follows as an attempt of its
  own, so a release whose disk links have all died still loads from the `.zip` beside them.
- Disk images are kept **whatever their format**: the disks of one set may be archived differently —
  Hardwired by The Silents & Crionics has side A as `.dms` and side B as `.adf`, and keying on the
  extension alone would silently fetch only one of the two.
- `IGNORED_EXTENSIONS = ["sid", "pdf", "rtf"]` are never the main file; loading the soundtrack
  because the demo 404'd is worse than failing. But a filter that empties the list is itself dropped
  and every URL becomes an attempt, so there is always something to fetch.

**Failure semantics.** `fetch_first_available` treats grouped URLs as alternatives — a dead/404/timed
out link only rules out that URL; every failure is logged and the *last* is returned, because it is
the one that ran out of alternatives. Each URL is itself tried against every mirror its link class
has (`fetch.rs`), so reaching the next URL means all of those failed. In a disk set every disk must
land — half a set won't boot — so a disk that can't be fetched at all fails the whole attempt, and
the caller falls back to the next download. `fetch` is a parameter so the walk can be tested without
a network.

**Progress**: a multi-disk set reports nothing, since forwarding each disk's byte count would restart
the bar on every disk. A single file falling back to the next URL *does* restart it, which is honest
— that transfer really is starting over somewhere else.

**`pick_download(name)`** narrows a URL-backed source to the first URL ending with `name`, for an
override that says which download is the demo (a demozoo release often lists the demo, its
soundtrack and a scan of the disk label side by side). It is the **tail of the whole URL** that
matches, not just the file name, so `name` can carry as much path as it takes to disambiguate:
`demo.zip` matches any link ending that way, `1997/demo.zip` only the one under that directory. A
name that matches nothing leaves the list alone and warns — the load falls back to guessing rather
than failing outright, which is what happens when a mirror renames a file out from under an override
written months ago.

`FileSource::resolve` blocks for as long as the download takes, so on the main thread it is only safe
for a source that is already a path.

`url_extension`/`url_file_name` work on the URL's *path*, so a `?query` or `#fragment` trailing the
file name can't be mistaken for an extension; the name is percent-decoded so it can be compared with
what an override names.

`Patch::bytes()` decodes base64 per load rather than up front: that is the form the toml carries and
the struct is built from, and a patch is a config file of a few dozen bytes.

An `EmuFile` can be: a single PRG/ADF/other; a parsed m3u for loading (Amiga or C64 with disks
listed, path = the m3u); a parsed m3u not supported for loading (no files listed, path = directory);
a directory (if leaf); or an archive (system type unknown).

## `fetch.rs`

**Timeouts.** `MAX_REDIRECTS = 10` (browser-typical). `CONNECT_TIMEOUT = 5 s` — a scene archive that
is up answers well inside this; one that is down otherwise leaves the connect hanging until the OS
gives up minutes later. `RESPONSE_TIMEOUT = 10 s` bounds only the time until the server *starts*
answering (HTTP response head, FTP control reply) — it deliberately does **not** bound the transfer:
large demo archives off a slow mirror are normal and must not be cut off mid-download.

**Cache budgets.** `SMALL_FILE = 1 MB` is where the download cache stops being small files and starts
being big ones — comfortably above a demo, a tune or a cracktro and below a disk image or CD track.
`SMALL_LIMIT = 250 MB`, `LARGE_LIMIT = 750 MB`. Small files are tiny individually but unbounded in
number, so without a cap a long collection browse fills the disk — and out of one shared budget,
thousands of them would evict every big download there was. Both are only defaults: the cache writes
them to a `.limit` file the user can edit.

**`LINK_BASES` — link classes, not URLs.** Demozoo does not store a URL for files it knows an archive
for: it stores a *link class* plus a parameter, and the db generator keeps that pair as
`SceneOrgFile:/parties/2006/assembly06/demo/x.zip` rather than resolving it (see demodb's
`demozoo.py`). Resolving here means a mirror dying — or a faster one appearing — is a change to this
table rather than a regenerate of every db file that mentions it. The URL is the base with the
parameter appended, so a base carries whatever trailing `/` or `?` the join needs. **Any class listed
is also a scheme demarc accepts as a URL, so keep the names distinct from real schemes.** Matching is
case-insensitive: the db spells the class the way Demozoo does, but a value that has been through
`Url::parse` (how db lines reach `fetch_url`) arrives lowercased.

**`MIRROR_ROTATION`** records, per class, which mirror to start from; the rest of the list follows
cyclically. A download that had to fall past a dead or slow mirror records the one that worked, so
later downloads of the same class start where the last success was instead of timing out against the
same broken host every time. The winner is stored as an **absolute index** rather than "rotate past
the ones we skipped", so a download finishing while another thread rotates the same class still
leaves the list pointing at a mirror known to answer. Memory only — a fresh run starts from the table
order.

**`URL_REWRITES`** — `(prefix, replacement)`, first match wins, applied *after* `LINK_BASES`, so a
rule must not undo a mirror choice made there (hence no rule for the listed bases). Current reasons:
- `funet`, `sndh` — plain http (or ftp) no longer serves these files.
- `scene.org` — a `/get/` link 302-redirects to a slow FTP mirror; the `/get:de-https/` variant
  serves the file over HTTPS directly. (`LINK_BASES` names that mirror directly for the same reason.)
- `modland` — some links already carry the `/pub/modules` prefix, giving a doubled path once the base
  is prepended.
- `untergrund` — the fujiology archive moved from user `ltk_tscl` to `ltk_tscc`.

**Downloading.** `download_to` tries every candidate URL in turn. `path` is the staging file
`FileCache` hands out and is only published under the entry's real name once this returns `Ok`, so an
interrupted download never leaves a truncated file masquerading as complete. Each attempt truncates
afresh, so a mirror that died mid-transfer leaves nothing for the next to append to; `on_progress`
restarts from zero each attempt, which is honest. Every failure is logged and the last is returned as
the one that ran out of alternatives.

**Redirects are followed manually**, not by `ureq`, so a redirect from an `http(s)://` URL to an
`ftp://` one — as files.scene.org does for its `/get/...` links — switches transport instead of
failing on an unknown scheme.

`Content-Length` is the *transfer* size, which equals the file size only when the body isn't
compressed. Scene archives are already-compressed binaries served as-is, so in practice it matches;
if one is ever gzipped the bar just tops out early.

**FTP.** Optional `user:password@` in the authority, anonymous otherwise; binary mode so files aren't
corrupted by line-ending translation. The path is **percent-decoded** before going out as the `RETR`
argument: FTP has no percent-encoding, so a server asked for `Count%20Duckula.png` looks for a file
with a literal `%20` and answers 550. URLs arrive encoded either from the db or because `Url::join`
encoded a redirect `Location` containing raw spaces — exactly what files.scene.org's `/get/...` links
redirect to. Connecting uses an explicit timeout rather than `FtpStream::connect` (which has none and
hangs for the OS default on a dead host), which needs a resolved `SocketAddr`, so DNS is done here
and the first address taken. The read timeout is set on the **control** connection only; the data
connection `retr` uses is a separate socket, so a large slow transfer is unaffected. Not every server
implements `SIZE`; without it progress stays indeterminate.

**Cache key vs. file name.** The cache entry is keyed on the URL **as the db writes it**, so a link
class keeps its cache entry when `LINK_BASES` changes mirror; the *name inside* the entry comes from
the resolved URL, which carries the file's real name and extension. `url_filename` derives a
filesystem-safe name from the final path segment, dropping `?query`/`#fragment` and percent-encoding
everything outside the URL-unreserved set (which happens to be exactly the set safe in a filename).
The segment is percent-**decoded** first so an already-encoded URL doesn't come back doubly encoded —
`Count%20Duckula.png` is a space, and re-encoding the `%` would name the cached file
`Count%2520Duckula.png`.

**`gather_files`** copies cache entries into one fresh temp dir so the disks of a set end up side by
side (the cache stores one entry per URL). Each copy keeps the cached file's URL-derived name, which
is what ends up in the generated m3u — so two disks of one set whose URLs differ only in a directory
land on the same name here; they stay apart in the cache, but this copy flattens them.

`on_progress` is not called at all on a cache hit, so a progress bar must not assume it will fire.
It is called once per write (every chunk `std::io::copy` moves) — cheap enough for an atomic store,
too often for anything expensive.

## `cache.rs`

A keyed, size-bounded store for files demarc downloads or derives and would rather not produce twice.
Content-addressed by a caller-supplied key: the key is hashed, the hash names a directory under the
cache root, and whatever the caller produces lives inside it. That shape means eviction has exactly
one kind of thing to evict, and a cache hit is a single `is_file` test.

Two entry shapes: `get_file` for a single produced file (a download, a built disc image) and
`get_dir` for a set of files that only make sense together (a cue sheet and the tracks it names).

**Both publish atomically** — build under a dotted `.part` name, rename into place. That is not
paranoia about crashes so much as about *other demarc processes*: two copies opening the same release
at once will race, and a half-written file under a name that says it is finished is a cache hit that
returns garbage forever.

**Two bounds, neither of which ever fails a lookup**: a size budget pruned back to, and an age limit
past which an entry stops counting as a hit.

- `STAMP = ".stamp"` records when an entry was *produced*. The age limit needs a timestamp a cache
  hit does not move, and the entry's own mtimes are the opposite: `touch` pushes them forward on
  every hit so eviction can tell unused entries from merely old ones. An entry that stayed popular
  would otherwise never be seen as stale. It is written **inside the staging directory**, so entry
  and stamp are published by the same rename. Failures are ignored: an unstamped entry reads as
  expired and is produced again, a wasted download rather than a failed lookup. A stamp dated in the
  future means the clock moved, not that the entry is from tomorrow — it counts as brand new, the
  reading that doesn't throw away a good entry.
- `LIMIT_FILE = ".limit"` holds the budget, written on first prune and read on every prune after, so
  a user has a file to edit rather than a constant to rebuild demarc over. One line per size band:
  a bare budget for the band holding everything left over, `<entry size>=<budget>` for the ones below
  it; `#` comments and blank lines allowed. An unparseable file is reported and **ignored** rather
  than treated as zero, which would empty the cache over a typo. `.limit` is never evicted as an
  entry — that would silently reset a limit the user set by hand.
- `PARTIAL_GRACE = 1 hour`. Pruning happens at startup when *this* demarc holds nothing, but another
  demarc may be halfway through a download; deleting the file it is writing turns its publish into an
  error. A live transfer touches its `.part` continuously, so an hour of silence means the owning run
  is gone. A timestamp in the future reads as in-flight — the answer that doesn't delete somebody's
  download.

**Size bands (`with_level`)** — without them a cache is a single pool and whichever kind of entry
arrives in bulk evicts the other: a browse through a few hundred tiny downloads pushes out big disc
images that cost minutes to rebuild, and one big image pushes out hundreds of small ones. Bands are
half-open (each covers entries above the next band down up to its own `max_entry`), so they can be
added in any order, and the budget passed to `new()` is the one for everything above the largest
named band. Pruning is one pass per band, each totalling and evicting only its own entries.

**Staleness is a fallback, not a failure.** A stale entry stays on disk while the replacement is
produced and is returned unchanged if producing one fails — being a fortnight behind beats not
running because the network is down. An entry carrying no `STAMP` (written before the cache had an
age limit, or by a run that died between publishing and stamping) counts as **too old** rather than
fresh: the cost of being wrong is one produce, and the fallback keeps even that from being fatal.

**`get_file`**: `filename` is *not* part of the key — it is the readable, correctly suffixed name the
entry carries, and downstream code dispatches on the extension, so the extension has to survive. A
`.part` left by an interrupted run is not resumable (whatever is in it came from a transfer that
never finished) so it is removed before the producer starts rather than appended to.

**`get_dir`**: an entry counts as present once `marker` exists inside it, so the producer's last
write should be the one file that proves the rest arrived. An entry without its marker never
finished; nothing may reference it, so it is cleared rather than renamed alongside. A stale entry
sitting on the name the rename needs is moved to `.<hash>.old` rather than deleted, so there is
something to put back if publishing fails — and if another demarc built the same entry meanwhile, its
copy is as good as ours by construction (same key, same contents), so ours is dropped.

**Pruning** is per entry (the unit a hit is keyed on), using the newest mtime inside it as last-use
time. Oldest first. `.part` files and empty directories are wreckage, not entries — neither can ever
be a hit, so neither is worth carrying until it happens to be the oldest thing here (once
`PARTIAL_GRACE` says nobody is still writing it). `get_file`'s staging file lives *inside* the entry,
so an interrupted download leaves one next to no payload at all (`sweep_partials`). Errors are logged
and skipped: a cache that can't be pruned is a disk-space problem, not a reason to refuse to start.

**`KeyHasher`** builds a key out of content rather than a short string — callers keying on what a
file *contains* (because the path is a fresh temp dir every launch) would otherwise concatenate
megabytes into a key string. **SHA-256, not `DefaultHasher`**, whose output is explicitly not stable
across Rust releases: keyed on that, a cache silently invalidates in its entirety on every toolchain
upgrade. Each field is **length-prefixed**, so `["ab","c"]` and `["a","bc"]` are distinct keys.
`hash_key` takes 16 hex chars (64 bits) — far past any plausible collision, and a key's own text is
not a safe directory name (it may be a URL, arbitrarily long, and `.../v1/game.zip` vs
`.../v2/game.zip` must not collapse).

`touch` opens the file for **write**, not read: on Windows `set_times` needs write access and on Unix
`futimens` wants a modifiable handle. Failures are ignored — the entry just risks being evicted
earlier than it should. `entry_stats` counts an unstattable path as zero-sized and last used at the
epoch, so a broken entry is evicted first.

`parse_size` accepts `K`/`M`/`G`/`T` (each 1024× the last) with an optional trailing `B`, so `500MB`
reads the same as `500M`; `format_size` writes back the most readable spelling that parses back
unchanged.

With no user cache directory the cache still works, it just lands somewhere the OS may clear between
runs — a slow demarc beats one that refuses to fetch anything.

## `files.rs`

**`leak(s)`** gives a runtime-built string the `'static` lifetime an `EmuFile` wants. The file list is
built once and kept for the whole run, so nothing in it is ever freed anyway; leaking says so in the
type and lets entries hold `&'static str` instead of `String`. Only used for the handful of strings
not already slices of the leaked db text — m3u tags, file stems, and the overrides read at startup —
so the leak is bounded by the size of the file list. `db_text` is the one *big* leak: a db is read
once, and slicing it beats copying every field of every line into its own `String`.

**db format.** Each non-blank line is `key:value` fields separated by tabs, in any order
(`id:1\ttitle:Zentro 4\tauthor:Zenith\t…`). Only the **first** `:` splits a field, leaving values
(URLs above all) intact. Every field becomes meta; `title`, `author` and the year (the first
`-`/`/`/`.`-delimited part of `date`) additionally fill in `GameInfo`. `download` becomes the entry's
path, fetched on demand. Lines with no URL are skipped. A db packed with gzip, bzip2 or Unix compress
is unpacked first.

A `# Platform:<name>` header line applies to every line below it, becoming `platform` meta on each
entry that doesn't name one itself (a `platform` field on the line wins). A header may carry further
`key:value` pairs (`# Platform:Amiga puae_model:A500`) which likewise become meta on every entry
below — a db can this way set emulator settings for all its lines at once. **Only a comment made up
entirely of `key:value` pairs is a header**, so an ordinary prose comment never turns into meta.

`parse_pouet_rank`: the field is `pouet:<cdc>,<thumbs>,<rank>,…` — pouet id, thumbs up, and ranking
position with 1 best. A release not on pouet has no field; one that is but unranked leaves the item
empty. Both give `None`.

**`DbFilter`** matches regexes against the **raw fields** of a line before it is parsed. A line must
match *every* include and no exclude, so repeating `-I` narrows and repeating `-X` widens what is
thrown away. Header comments are always read, so their platform and meta still apply to survivors.
Matching field by field keeps a pattern inside the field it names — `author:.*Firefox` can't run past
the end of `author:` and pick up a `Firefox` later on the line — and lets `^`/`$` anchor to a field,
so `^category:Demo$` picks plain demos while leaving `category:Demoshow` alone. The flip side is that
a pattern can no longer span two fields.

`collect_db_stdin` lets entries be filtered before they reach demarc
(`grep Amiga bitworld.txt | demarc`). It does nothing when stdin is a terminal — reading would just
block waiting for the user to type a db.

`collect_files_`: rule of thumb — collect only *cheap* information, since there can be a lot of
files. An m3u stops recursion. Images on disk are assumed to be screenshots. A directory holding
nothing but disk images is left to the loader, which mounts the whole set rather than each image on
its own.

## `m3u.rs`, `workfile.rs`

`M3u::relocate` writes the m3u and copies all files into the same directory; `target` must have a
parent. `M3u::verify` checks all listed files exist (relative to the given parent, unless absolute).

`WorkFile` passes around files or dirs that may be temporary. `temp_dir`, if `Some`, must be the
parent of (or equal to) the path. `make_temp()` ensures the path is in a temp dir and can be
modified. `with_path` repoints while keeping the same temp dir alive (e.g. after unpacking, to reach
the file inside the extracted directory). `into_parts()` splits path and temp dir — **the caller must
hold the `TempDir` or the path stops existing**; the `From<WorkFile> for PathBuf` impl drops it.

## `load_error.rs`

`load` returns `anyhow::Result`, which keeps every `?`/`.context()` cheap but leaves the caller with
only a message. Since anyhow preserves the source chain, the concrete error is still in there — so
rather than convert the load path to a typed error end to end, the display boundary walks the chain
and downcasts. `classify` is the one place that knows how each failure looks, so the HUD, the log and
any future caller agree on what counts as "not found" or "offline".

Failures raised by demarc itself (no core, missing BIOS, unknown system) have nothing to downcast to
unless typed, hence the small error structs — a `bail!("…")` string would only be matchable by
substring, which breaks the moment someone rewords the message.

`LoadFailure`: `NotFound` (server answered, file isn't there — HTTP 4xx), `Timeout`, `Offline`
(couldn't reach the host at all), `DownloadFailed` (reached the server but the transfer failed —
protocol error, truncated body, 5xx), `Other`.

Gotchas encoded here: a failed **DNS lookup arrives as `Io` with an uncategorized kind** rather than
the `HostNotFound` variant you'd expect, so both land in `Offline` — as does a refused connection or
a mid-transfer reset. `Uncategorized` is unstable to match by name, so it is recognised by message.
FTP: 550 and other 5xx replies to `RETR` mean the path isn't there. `reason()` is kept to a few words
because the HUD is one line over the emulator output.

## `libloader.rs`

Downloads cores from `https://buildbot.libretro.com/nightly/<system>/latest/<name>_libretro.<ext>.zip`
where `<system>` is `linux/x86_64`, `apple/osx/arm64` or `windows/x86_64`.

**`ALT_SOURCES`** are cores the buildbot doesn't ship, paired with the base URL of the release that
does. Each holds one zip per platform named `<name>_libretro-<system>.zip` containing the library
under the same name the buildbot uses, so nothing downstream has to know the difference.
`alt_system()` is the platform segment those archives use (they name platforms their own way).
The gamescope core is the one that ships a **program** as well as a library: its zip holds the
gamescope compositor and its private libraries beside `gamescope_libretro.so`, and the core finds
them through the path this module hands it. Linux only, so `alt_system()` answering for another
platform only means a 404 and a warning — nothing there can run a gamescope session anyway.

**`$DEMARC_CORE_DIR`** is a colon-separated list of directories searched for `<name>_libretro.<ext>`;
first hit wins and takes precedence over the buildbot. Nothing else in the cache path runs, so a
local build is never written to (or evicted from) the cache. Exists so a core built from source can
be tested without overwriting the downloaded copy.

The `CORES` cache is keyed on the **URL**, not the core name, so the platform is part of the key and
a cache copied between machines can't hand back a library for the wrong one. **The library itself is
the `get_dir` marker**: an archive that unpacked to anything else is not a core and must not be
cached as one. `CACHE_LIMIT = 500 MB` (a dozen or so cores at a few tens of MB, with room for ones a
user tried once — those are what eviction is for). `MAX_AGE = 14 days`: the buildbot URL names
`latest`, so an entry keyed on it goes out of date on its own schedule; a fortnight keeps up with
upstream fixes without making a launch depend on the network, and `with_max_age`'s fallback keeps the
already-downloaded copy when the network isn't there. `get_libretro` returns `None` only if there is
nothing usable at all — a failed download with a cached core still returns that core. macOS: the
quarantine attribute is cleared so the result can be `dlopen`ed without a Gatekeeper prompt.

## `system_dir.rs`

`SYSTEM_CHECKSUM = env!("SYSTEM_ZIP_CHECKSUM")` — SHA-256 of `system.zip`, computed by `build.rs`
when the archive is packed. Prefers a local `system/` in debug builds, otherwise extracts the
embedded zip into the user cache. The path is run through `strip_verbatim_prefix`: cores get it (and
everything derived from it) as their libretro system/save directory, so it has to be one a C library
can open — not the `\\?\` form `canonicalize()` hands back on Windows.

## `utils.rs`

**`strip_verbatim_prefix`** — nothing but Win32 itself understands `\\?\` paths. A libretro core
reaches the filesystem through the C runtime and its own path joining, and neither copes: amiberry's
ROM scan `opendir()`s the directory it is handed and on a `\\?\` path that call fails outright, so it
finds no Kickstart, boots a romless machine and renders a black screen. (Its path joining also uses
`/`, which a verbatim path does **not** accept as a separator — under `\\?\` the string goes to the
object manager unparsed.) Hand out plain `C:\...` paths. A verbatim UNC path becomes
`\\server\share`; no-op elsewhere.

**`strip_lha_comment`** — Amiga LHA archivers store a file's comment in the header's filename field
after a `nul`. The reader only honours that convention when the header names Amiga as its OS, which a
level 0 header has no room to do, so the comment arrives glued onto the name with the `nul` escaped
as `%00` (`dcs-nons.exe%00from _Shape (@b112b.mtalo.ton.tut.fi)`). The name is cut back at the `nul`,
escaped or literal, the way `lha` itself does.

**Unpacking.** Single-file compressors (`.Z`/`.gz`/`.bz2`) carry no name for their payload, so it is
derived from the archive's stem (`demo.tar.gz` → `demo.tar`). `unpack_if_packed` is the bytes-only
variant for a data file packed on its own (a gzipped db). Only normal path components are kept, so an
absolute path or `..` in the archive can't write outside the target; a name that is entirely unusable
is skipped. Some formats (rar) mark directories only in per-file metadata the unified reader doesn't
expose, but always decompress them to nothing — so an **empty entry is treated as a directory**,
which both handles those and keeps a zero-length file from blocking a later `dir/child`. Tar is often
falsely reported by content detection, so it must also match by path.

### `sort_disks` — the disk-numbering rules (written out in full in the source)

Decide each disk's number: the digit just before the dot if there is no digit before it; **or** the
uppercase letter before the dot if not preceded by an uppercase letter; **or** the last standalone
digit in the name unless it opens the name.

```
disk3.adf       -> 3
45degreesA.adf  -> 1
3witches.dsk    -> None   (a title, not a disk number)
game_B.DMS      -> 2
GOA.dsk         -> None   (end of a word already in capitals)
```

If there are gaps (normally slot 1, because the first disk of a set is often named without a number)
fill them from the unsorted pile, preferring a name that starts like the first sorted disk:

```
space.adf, Space_disk2.adf, extra.adf  ->  space.adf takes slot 1
```

When two names claim the same slot, **digits beat letters**:

```
disk_A.adf disk_1.adf disk_B.adf disk_2.adf  ->  [disk_1.adf, disk_2.adf]
```

If *every* disk lands in the same slot the numbering is meaningless, so sort names normally instead:

```
intro3.adf, credits3.adf  ->  [credits3.adf, intro3.adf]
```

Names are walked in a fixed order first, so claims the rules can't separate always resolve the same
way whatever order the directory listing arrived in.

## `cbmconvert.rs`

Thin FFI wrapper: the C sources under `cbmconvert/` are compiled into the binary by `build.rs` with
`main` renamed to `cbmconvert_main` (`-Dmain=…`), so it is called directly rather than spawned.

**It is not a library.** `cbmconvert_main` relies on file-scope state and writes output files
relative to the current working directory, exactly as the CLI does — treat it as one conversion
invocation per call, not reentrant. `CWD_LOCK` serialises the working-directory switch (the CWD is
process-wide, so two conversions at once would write into each other's directory) and also keeps the
file-scope state to one conversion at a time. `CwdGuard` restores the previous directory on drop, and
the lock is dropped *after* that, so the next conversion starts from a known directory. A poisoned
lock is fine to keep using: a conversion that panicked still had its CWD restored by the same guard.

## `speed_test.rs`

Three phases. **Phase 1** waits for the core to actually start stepping frames (excluding core
creation and boot-to-first-frame), falling back after a grace period for cores that never report a
frame count. **Phase 2** warms up `SPEED_TEST_WARMUP = 1.0 s` so one-time setup — core boot,
render-pipeline/shader compilation, first-frame allocations — finishes before the clock starts.
**Phase 3** measures `SPEED_TEST_MEASURE = 2.0 s`, prints the frame count and requests app exit.
`done` guards against printing/exiting twice.

## `egui_ui.rs`

The HUD, the corner texts and the fuzzy-search file picker.

**The stale-modifier problem (recurs in every egui dialog).** egui only learns about a modifier
through the key events it is fed, so a modifier pressed here and released while another window had
focus stays "held" for good — and every exact-match lookup against `egui::Modifiers::NONE` then
quietly stops matching, which is what leaves the picker unable to see a plain arrow key again.
`live_modifiers()` reads the modifiers from Bevy instead (Bevy clears its keyboard state outright on
`KeyboardFocusLost`, so it recovers by itself), and `sync_modifiers()` overwrites both egui's running
state *and* the modifiers stamped on the events still queued for this frame, so the search box drawn
afterwards resolves its own shortcuts against the live keyboard too. `take_key()` counts and removes
every press of a key **whatever modifiers came with it** —
`egui::InputState::count_and_consume_key` insists on an exact modifier match, which is one stale
modifier away from dropping the key.

**Modality.** `HudState::open_dialogs` is a **count**, not a flag, so closing one dialog while another
is still open does not hand the keyboard back to the emulated machine; it lives on `HudState` rather
than the generic `SettingsState<T>` so `modal()` can answer without naming the settings type. Each
dialog reports each transition once. `modal()` is what the callers feeding keys to the emulated
machine check — a settings dialog with a focused text field would otherwise type into the emulator.

**Fonts.** `font.ttf` is loaded through the asset server rather than read from `system_dir` directly,
so egui picks up the very same face — and the same hot-reloaded bytes — as the Bevy UI. egui owns its
font bytes (it re-parses them for its own atlas), so the setup copies out of the Bevy asset instead
of sharing the `Blob`. The app font goes at the **front** of the family list (primary), with egui's
own fonts behind it as fallbacks for glyphs `font.ttf` lacks.

**Picker layout.** Fixed `ROW_HEIGHT` so the box doesn't resize as the list is filtered or emptied;
`INFO_LINES = 5` reserved so the centred layout doesn't jump as the selection moves between items
whose info differs in length; `LIST_HEIGHT_FRACTION = 0.6`. Width is the screen *height* capped to
what fits (the picker opens over a 4:3-ish emulator view), and the panels below take their width from
that one. `visible_rows()` is also the PageUp/PageDown step, so the key moves exactly one screenful.

**Picker behaviour.** The search box keeps focus the whole time the picker is up — egui only routes
key events to a focused widget, and a click on the emulator behind would otherwise take focus away
and leave typing going nowhere. Selection keys are consumed **before** the search box is drawn so the
`TextEdit` never sees them; Home/End are deliberately left alone and stay cursor movement inside the
query. Presses are *counted* rather than tested, so a held-down arrow keeps moving at the key repeat
rate even when several repeats land in one frame. The selection is clamped (not wrapped) and
re-clamped against the current length in case the list shrank. A new filter starts at the top; a
re-open keeps where the user was. The source is asked only when the query actually changed (the box
is polled every frame) or the list was just reopened, since the source itself may be a new one by
then. The info field asks the source only when the highlighted item changes, and stays hidden while
there is nothing to say.

**Row fades** live in egui's animation map keyed by **absolute row**, not viewport position, so they
follow their row when the list scrolls and keep decaying on wall-clock time while scrolled out of
sight. `animate_value_with_time(..., 0.0)` snaps the stored value so the highlight appears at once
and the fade later starts from full. `FADE_SECS = 0.5` — a row the user passed over dims out instead
of blinking off.

**`heading_with_shadow`** lays the text out **uncoloured once** so the single galley can be painted
twice with `Painter::galley` filling in a colour each time (a `LayoutJob` rather than
`layout_no_wrap`, which has no say over `halign`). Two `Ui::heading` calls would stack the shadow
*below* the text instead of behind it. The shadow is part of the allocated size, so the corner it is
anchored in leaves room for it, and it is black at the text's own alpha so shadow and text fade
together. `SHADOW_OFFSET` is a fraction of the font size so the look survives every `HEADING_SIZE`
scale.

**Toasts**: an empty text retires whatever is showing in a corner. The entry is **kept with its
duration ended** rather than dropped, because the fade-out animates on the text's own id — remove the
entry and the text blinks out instead of fading.

The download counter is a placeholder for a real progress bar (the byte counts behind it are already
tracked in `Emulator::load_progress`); it has its own colour and a smaller size than the corner HUD
texts, since it is status, not a title.

`ListSource = Arc<dyn FuzzySource<EmuFile>>` — every source agrees on one type, so a caller holding a
selection's `item` can ask the source for the entry (`get_data`) instead of keeping its own copy of
the list. A source whose rows aren't entries (the hotkey list) inherits the default and hands nothing
back.

`rasterize_svg`: `tiny_skia::Pixmap` stores premultiplied RGBA, `egui::ColorImage` wants straight
RGBA, so each pixel is unpremultiplied.

## `commands.rs`

Hotkeys (RightAlt/RightCtrl + key) → `Cmd` → app actions. One `Cmd` variant per `HOTKEYS` entry; the
glyph shown in the RightAlt overlay is derived from the trailing letter of the `KeyCode` (all hotkeys
are `Key*`).

`FILE_PICKER_ID = 1`, `DOWNLOAD_PICKER_ID = 2` — echoed back in `FuzzyListSelect` so the two lists'
selections are told apart. The picker's `item` is a stable index into `settings.files`, independent
of the current search filter.

**Shift+Enter in the file picker** opens a second list over that entry's download URLs. An entry's
URLs are alternatives (a mirror, the same release packed differently) and a plain load takes whichever
answers first; this lets the user say which one. Picking one **narrows the entry down to that single
URL**, or `FileSource::resolve` would re-apply its own idea of which to take. The picker's snapshot
still holds them all (`original_file`), so the entry can be pointed at a different download later —
`FilePickerSource` snapshots `settings.files` when first built.

`DownloadSource` shows the file-name part of each URL with the **whole URL in the info field**:
mirrors of the same release often share a file name, and the host is what tells them apart. It is the
one place a URL has to be taken apart rather than shown, so the one place worth parsing them.

**`trunc_url`** shortens by dropping path components from the left, keeping the two parts that
identify a URL — the host and the file name:

```
https://ftp.example.org/pub/demos/c64/1992/zentro4.zip
→ https://ftp.example.org/.../1992/zentro4.zip
→ https://ftp.example.org/.../zentro4.zip
```

A URL still too long once every component is gone is cut out of the **middle** instead
(`middle_cut`), keeping its head and the end of the file name including the extension. The path search
starts after `://` so the scheme's own slashes don't count as the first one.

**Multi-emulator hotkeys** (cycle, maximize, select all) are gated on `count > 1`: they are about a
*grid*.

Other notes: the fixed integer scales are CLI-only — the keyboard cycle returns to the
aspect-preserving modes. The fullscreen hotkey also writes `demo_settings.fullscreen`, so the settings
dialog opens showing where the window actually is and doesn't undo it on the next apply; it is the
only hotkey that moves a field the dialog also owns. The trigram index is built once on first open
and reused (indexing the whole list is what made reopening the picker slow); the clone is an `Arc`
bump. The info field's wrap width comes from the window height, which is how wide the list box is.
`SELECT_MENU_DELAY = 5` frames before `--select` opens the picker: the window is still settling on
its final size, and the picker's row count and width derive from it (the counter saturates so it
never wraps back to the trigger value). Known issue in the code: "We sometimes get quick
PRESS/RELEASE/PRESS for only press".

## `fuzzy_list.rs`

Pure filtering logic — no UI, no engine. Matching is behind a `FuzzySource` so the strategy is
pluggable. `SubstringSource`/`AllWordsSource` do a case-insensitive substring scan over an in-memory
`Vec<String>` with lowercased copies precomputed — fine up to a few thousand entries.
`AllWordsSource` splits the query on whitespace and requires every word as a substring in any order,
so `"na an"` matches `"banana"`.

An **id** is a stable handle on an item in the source (a `Vec` index, a db row id) — it is what a
selection reports, so it holds regardless of the current filter/order. An empty/whitespace query
returns the head of the full list. `get_text` is asked only for rows on screen, so a source may build
it on the spot. `get_data` is **borrowed**, not cloned: the picker asks as the selection moves and a
record can be much bigger than a row. The bundled sources implement the trait for *every* `T`, so a
plain list of strings drops into a picker whose other sources carry records — that is also why their
`search` is an inherent method as well, so a caller holding the source needn't say which `T` it means.

### `IndexedSource` — prune then rank

1. A precomputed inverted **trigram index** maps every 3-byte substring to the sorted ids of items
   containing it. A query word jumps straight to the handful of items that could contain it
   (intersecting its trigrams' posting lists) instead of scanning all items. Measured **~14x overall
   and 13–27x on selective multi-word queries over 40k entries** versus the linear `contains` scan —
   those are exactly the case where a linear scan must touch *every* item because few match.
2. The small surviving set is verified against the real substring predicate (**trigram membership is
   necessary but not sufficient**) and then ranked by `nucleo_matcher` (the matcher behind Helix), so
   results come back best-first rather than in id order.

Details: UTF-8 is self-synchronizing, so byte trigrams are a sound necessary condition for a `str`
substring match, and the verify step re-checks with real `str::contains`. Ids are pushed at most once
per trigram and in increasing order, so posting lists stay sorted. Intersection goes shortest-list
first so the working set only shrinks. Words shorter than a trigram (1–2 chars) can't be indexed, so
a query made only of those falls back to a linear scan — fine, because such queries match many items
and hit `RANK_CAP` almost immediately. `RANK_CAP = 4096` bounds the ranking work so a keystroke stays
well under a frame; matches past the cap are ignored, since a picker gets narrowed by typing more,
not by scrolling thousands of rows. `DEFAULT_MAX_RESULTS = 500_000`. Cheaply clonable (`Arc`) —
building the index is the expensive part, so a caller reopening the same picker builds one and clones
per open.

## `egui_settings.rs`

A settings dialog you get by declaring a struct: hand `ShowSettings` any `#[derive(Reflect)]` struct
and this draws a modal panel with one row per field, the editor kind derived from the field's *type*.
Every edit reports the whole edited struct back as `SettingsApplied` **as it is made**, so a change
is live immediately and closing has nothing to confirm or undo. Nothing is written for the caller.

**Why reflection and not serde or a bespoke derive**: `bevy_reflect` is already compiled in (a
non-optional dependency of `bevy_ecs`), and it is the only one of the three that hands over an enum's
*variant list* (`EnumInfo::variant_names`) — exactly what a ComboBox needs and what a `Serialize`
pass cannot see. The module knows nothing about what it edits, and is split the way `fuzzy_list` is
split from its drawing: `describe` and `set_variant` are pure functions over `dyn PartialReflect`
with no egui and no `App` in sight, and the tests exercise those.

- **`widget_for` order matters.** `Color` is itself a reflected *enum* (over colour spaces) and
  `Option<T>` is an enum with a payload-carrying `Some`, so the concrete types must be recognised
  before the generic enum arm — and that arm insists every variant is a unit variant
  (`all_unit_variants`; `Option<T>` and `ScaleModeArg::Fixed(f32)` fail it). The **type info** is the
  authority on the variant list; the value only says which is live.
- `i128`/`u128` are deliberately absent from `is_int` — egui has no `Numeric` impl for them, so they
  fall through to `Widget::Unsupported` rather than a drag that can't be drawn. `Unsupported` is
  drawn as a greyed-out row rather than skipped, so a field that silently cannot be edited is visible.
- `Range::new` picks a drag speed that crosses the span in roughly 300 px — about a third of the
  panel width, so the whole span is reachable in one drag without skittering. Bounds are generic so
  an integer field can be given integer bounds; every integer type narrower than 53 bits converts
  losslessly, `u64`/`i64` bounds must be written as `f64` literals (no worse than the precision
  `DragValue` works in anyway). Floats default to 0.1 per pixel — fine for the 0..1-ish factors this
  app is full of, without making a large value take a mile of dragging.
- Labels come from field names (`idle_timeout` → `Idle Timeout`) because doc comments would
  be better but `NamedField::docs` sits behind bevy's `reflect_documentation` feature.
- Colour: read out as sRGB bytes whatever the field's colour type, edited by egui, written back in
  the field's own colour space.
- The combo box uses `selectable_label`, not `selectable_value`, which would force `PartialEq` on
  every settings enum — three of the ones in `config.rs` don't derive it. The pick is taken inside
  the closure and applied after, because the combo box holds the borrow while open.
- `settings_body` is deliberately **not generic**: it works through `dyn PartialReflect` so a dozen
  settings types share one copy and only the thin ECS wrapper is monomorphised.
- `scale_widgets` exists because `setup_egui` only overrides Heading and Body, leaving Button (what a
  `DragValue`/`Button` labels itself with) at egui's default 14 pt, unreadable in this app's
  1600-tall virtual space. Spacing has to grow with it or widgets stay letterbox-thin around the
  bigger text. All the size constants are in that 1600-tall virtual space, hence much larger than
  egui's defaults.
- `WIDGET_WIDTH` is fixed so rows line up and the panel doesn't resize as a combo box's text changes.
  The close button is placed against the *drawn* panel rect (the panel is only as wide as its widest
  row, unknown until drawn); the title row has already reserved `CLOSE_SIZE` so they cannot collide.
- The UI system runs `.after(update_ui)`, which is what sets `pixels_per_point` for the frame —
  drawing before it would size the first frame wrongly.
- Opening two dialogs over one type in a frame is a caller bug, so the last writer wins rather than
  queueing.

## `demarc_settings.rs`

The app's own settings struct (RightAlt+E) and the system that applies it. The resource is seeded
from the command line and thereafter holds what was **last applied**, which is both what a fresh open
shows and the baseline `apply_settings` compares against.

`apply_settings` goes **field by field against the last applied value** rather than writing them all:
the dialog edits a snapshot, and a field it never touched must not clobber what something else did
while it was open — RightAlt+F moving the window, say.

`latency` is the frames a core's worker may run ahead; it takes effect on the *next* release loaded
(`NewSys::set_meta`), so the change is announced in the HUD because there is nothing to see. `0`
would be a rendezvous channel (worker blocked until the frontend takes each frame), so the range
starts at 1. `volume` is TBD — nothing reads it yet.

## `shader_dialog.rs`

The post-process shader picked from a *collection* combo box and, under it, one combo box per
wildcard of that collection's pattern.

The collections come from `shaders/shaders.toml` (see `docs/SHADERS.md`): one table each, whose
`pattern` says both where the presets are and how their paths read. Every `<Tag>` is a wildcard and
becomes one combo box, named after the tag:

```
[Commodore]
pattern = "Mega_Bezel_Packs/TheNamec-Commodore/presets/<System>/<Monitor>/<Shader>/<Type>_<Time>.slangp"
```

That layout is far too big for the fuzzy list (the Commodore pack alone holds ~72k presets) and not
something `egui_settings` can draw either, since its combo boxes come from a reflected enum's variant
list while these choices are only known once a directory has been read. So the tree is walked
**lazily**, one `read_dir` per path component as the boxes above it change, and only a few dozen
names are ever on screen. `PresetBrowser` is the whole of that logic and knows nothing about egui;
the tests exercise it against a tree they build. A pick takes effect the moment it is made — composed
back into a path and written to `ShaderPath`, which the render world extracts. The dialog chrome
(panel metrics, widget scaling, close button) is the settings dialog's, so the two look like one
dialog with two contents.

- `CONFIG_PATH = "shaders/shaders.toml"`, looked for under the working directory first and then
  beside the executable; the file's own directory is what its patterns are relative to. The working
  directory is the **empty path**, so what is built on it stays relative — and so stays copyable into
  a `--slangp` argument. `toml`'s `preserve_order` feature is what keeps the collections in the order
  the file writes them.
- `Segment` is one `/`-separated component of a pattern: the literal text around its `<Tag>` holes,
  plus a regex built from them. Wildcards never cross `/` and are **lazy** but for the last one of a
  component, which takes what is left over — `MBZ__<Level>__<Type>.slangp` reads
  `MBZ__0__SMOOTH-ADV__GDV.slangp` as `0` + `SMOOTH-ADV__GDV`. `literals` is kept so `compose` can
  put a selection back together, which is how `path()` is built.
- One level per tag, flattened across the segments in order (`starts` maps segment → first level).
  A component with two tags is listed **once** and shared by both boxes, and the second box offers
  only what goes with the first — that is what keeps `NEAR` from leaving a `FLAT_DAY` selected under
  a monitor that only ships `CURVED_*`.
- Only the levels **below** the one that changed are re-read, and a level keeps its pick if the new
  choices still contain it — so walking through the monitors of one machine stays on the same flavour
  and preset rather than resetting each time.
- `label`: a shouted name from a *file* reads as words (`NEAR_CURVED` → `Near Curved`), a shouted
  *directory* name is a code and is left alone (`MBZ_SHARP_STD`), and anything with lowercase in it is
  already written the way its author meant it, bar the underscores a directory name uses for spaces
  (`Commodore_Amiga500` → `Commodore Amiga500`).
- `reveal` moves the selection onto a given preset when the dialog opens, so it comes up showing
  what's on screen rather than the first preset of the collection. `contains` decides which collection
  to open on — including a path of the collection's *shape* naming a preset it no longer ships, which
  still belongs there rather than to the default collection. `strip_root` tries the path as given first
  (so a browser on a relative root recognises a relative path) and through `canonicalize` after
  (which matches an absolute `--slangp` against a relative root).
- A collection whose pattern matches nothing on disk is left out rather than offered as a row of empty
  combo boxes, so a checkout with no `shaders.toml` shows the default collection alone.
- The default collection (index 0) is whatever `--shader` names — one preset, no levels, so it draws
  the collection row alone; the grid grows and shrinks with the collection picked. Picking a preset is
  asking to see it, so it switches `crt_effect` on. `--shader none` is the stock passthrough preset
  with the effect off, so the path line names what it does ("no effect") rather than what it does it
  with.

## `audio.rs`

**`AudioResampler`.** The core hands a variable number of frames each call while `FastFixedIn` wants
a fixed input chunk, so incoming samples are deinterleaved into per-channel buffers and consumed a
full chunk at a time. `FastFixedIn` is an **asynchronous** resampler whose ratio can be nudged with
`set_resample_ratio_relative` without rebuilding — that is what makes per-frame drift correction
cheap; only a genuine change of the core's *nominal* rate triggers a rebuild.

- `MAX_RATIO_ADJUST = 0.05` is the relative-ratio headroom the resampler is built with, kept well
  above the controller's own clamp so it never saturates here.
- `set_adjust`: positive raises the effective input rate, so **fewer** output frames per input frame
  and the ring buffer drains; negative does the reverse. `rel = 1.0 / (1.0 + adjust)`. Ramped over
  the next chunk to avoid zipper noise.
- `from == 0` means the core hasn't reported a rate yet — keep the current resampler rather than
  rebuilding with a bogus ratio.
- On a rate change the old resampler is **flushed first**: `process` always drains down to a
  sub-chunk remainder, so the buffer holds fewer than `chunk_size` frames; that remainder is
  zero-padded to a full chunk and pushed through the old resampler, so the captured frames (and the
  previous chunk's delayed tail) come out while the padding zeros land in the discarded next block.
  Without this a rate change would drop or mis-pitch already-captured audio.

**`pick_output_config` — the WASAPI 5512 Hz bug.** Every advertised range must be *clamped against*,
not just filtered with `min_sample_rate() <= target`: since cpal 0.17 the WASAPI backend no longer
asks `IsFormatSupported` and instead advertises **each entry of cpal's `COMMON_SAMPLE_RATES` as its
own single-rate range**, letting the audio engine convert via `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM`.
The first of those is 5512 Hz, so picking the first range with `min <= target` and then taking
`target.min(max)` opened the output stream at 5512 Hz on Windows.

The target rate is **whatever rate the device itself runs at**: nothing played here is sourced above
44.1 kHz, so there is nothing to gain from asking for more — and on WASAPI the device rate is the
shared-mode mix rate, so matching it keeps Windows' audio engine from resampling on top of us.

**Buffer size**: prefer 2048 frames (the resample ratio is continuously adjusted from ring-buffer
fullness, so a small buffer gives tight feedback), clamped into the device's advertised range — if
the smallest supported is larger, take that; if the range is unknown, let the backend choose. cpal's
CoreAudio backend rejects out-of-range fixed sizes with `StreamConfigNotSupported`, so the clamp is
what keeps macOS happy.

**`SendStream`** wraps `cpal::Stream`, which cpal marks `!Send + !Sync` because a few backends need
the handle to stay on its creating thread. Nothing is ever called on it — the SAFETY argument is that
the stream handle is safe to move and drop across threads and is never accessed after
`init_audio_stream` returns it.

## `media_keys.rs`

MPRIS listener on Linux (D-Bus `MainInterface` + `MediaPlayer` at the standard MPRIS path), compiled
to a stub elsewhere. On non-Linux a dummy thread keeps the channels alive by reading from them, so
the API is the same on every platform. `commands.rs` holds the sender end alive (even though nothing
pushes state to it) so the listener's channel stays open, and maps Next → next file, Play/Pause →
toggle pause, mirroring the Ctrl-N / Ctrl-P hotkeys.

## `screensaver.rs`

Suppresses screen blanking while the emulator is fullscreen. On Linux it **prefers the Wayland
`zwp_idle_inhibit_manager_v1` protocol**, answered by the compositor itself, and falls back to
`org.freedesktop.ScreenSaver` over D-Bus when not on Wayland. The D-Bus path needs a daemon to own
the well-known name, and a compositor-only session may well have none — Hyprland under Omarchy 4
dropped hypridle, which used to provide it, in favour of a quickshell idle monitor watching
`ext-idle-notify-v1`. No-op on other platforms.

**Fullscreen detection is platform-split for good reasons.**
- `window.mode` is the primary signal **on macOS**, where `covers_a_monitor` never matches: a
  fullscreen `NSWindow` is sized to the visible frame (screen minus menu bar), not the monitor's
  physical bounds.
- On **Linux** `window.mode` is unreliable: under Wayland winit leaves it at a stale
  `BorderlessFullscreen` after the window is toggled back out, which would keep us inhibited forever
  — so there only geometric coverage is trusted.
- `covers_a_monitor` also catches a compositor (Wayland tiling WMs like Hyprland) fullscreening a
  window on its own while `window.mode` stays `Windowed`. On X11 winit reports the window's physical
  position so a proper rectangle-cover test is possible; on Wayland the position is never reported
  (stays `WindowPosition::Automatic`), so it falls back to an exact size match against a monitor —
  which is what a fullscreened window produces.

**The Wayland inhibitor.** winit's `wayland-client` objects aren't reachable, so a **second
`Connection` is built over the same `wl_display`** — libwayland is designed for this: the extra
connection gets its own event queue and never sees winit's events — and the window's `wl_surface`
pointer is re-wrapped as a proxy on it. The inhibitor has to name a real, **mapped** surface
(compositors ignore ones anchored to an unmapped surface), so a throwaway surface of our own wouldn't
do. SAFETY rests on winit keeping the `wl_display` alive for as long as the window exists, and
`from_foreign_display` recording that we don't own it, so dropping our `Connection` won't disconnect
it. The surface address is kept as an **integer**, only ever compared and never dereferenced, which
keeps the resource `Send + Sync`; a window teardown/rebuild (or compositor restart) invalidates the
surface an inhibitor is anchored to, so the backend is dropped and rebuilt against the new one. After
issuing requests a roundtrip is done: requests are buffered, and it also drains our own queue and
surfaces a protocol error here rather than at some unrelated later point.

State bookkeeping: the state we last *asked* for is tracked separately from the backend's own, so a
failing request isn't retried and re-logged every frame; the warned flag is cleared on success so a
later, genuinely new failure is still reported once. With no window handle yet, the requested state
is left unlatched rather than picking a backend blind. `set_inhibited` is idempotent, so calling it
every frame only produces D-Bus traffic on an actual change.

## `mouse_cursor.rs`

`HideMouse` is set by `setup_retro` for layouts that fill the window with emulator output and nothing
else. **`--grid` leaves it clear**: there the pointer picks which view has focus, so it must stay
visible.

**macOS uses Quartz, not winit.** Bevy/winit's `CursorOptions::visible` maps to `NSCursor
hide`/`unhide`, which the window server keeps re-asserting via its cursor-rect mechanism for a
borderless-fullscreen `NSWindow` (there is no real fullscreen space to anchor it to), so the arrow
reappears the moment the mouse moves. `CGDisplayHideCursor`/`CGDisplayShowCursor` hide it at the
display level instead. Those are **refcounted** per Apple's docs, so the last state told to Quartz is
tracked and repeated calls are no-ops — calling `Hide` every frame without a balancing `Show` would
need an equal number of `Show`s to ever bring the cursor back.

## `pixels.rs`

`RGB565_LUT` and `RGB1555_LUT` are precomputed 256 KiB rodata tables indexed by the raw 16-bit pixel;
each entry is a `u32` whose native bytes are `[r, g, b, 255]`. They replace per-pixel bit unpacking
in `video_refresh`.

**`convert_xrgb8888` — the spelling is the point.** The source is BGRA in memory (little-endian
XRGB8888), so every pixel is a pure byte permutation: as a `u32` it is `0xAARRGGBB` and we want
`0xAABBGGRR`, which is `swap_bytes()` then `rotate_right(8)`. Written that way LLVM recognises
bswap+rotate as a byte shuffle and vectorizes it; the equivalent
`from_ne_bytes([px[2], px[1], px[0], px[3]])` stays scalar at four `movzbl`s, three shifts and three
`or`s per pixel. **Measured on a 320x240 frame: 37 µs scalar, 11 µs with baseline SSE2 (two shuffles
plus a shift/or per 4 pixels), 3.3 µs with SSSE3 (one `pshufb` per 4), 2.4 µs with AVX2 (one
`vpshufb` per 8).** The loop is indexed rather than `iter_mut().zip(chunks_exact(4))` on purpose: the
`Zip::new` call doesn't get inlined without LTO, and an unvectorized loop body is the entire cost.
Since we ship a baseline x86-64 binary, `pshufb` is only reachable through **runtime dispatch** —
`is_x86_feature_detected!` caches its answer in an atomic, so the per-frame cost is a relaxed load.

## `wine.rs`

How a Windows release is *started* under wine: the command, the prefix, and shutting that prefix down
afterwards. **Nothing here runs a demo** — the running is the gamescope libretro core's
(`external/gamescope/src/libretro/`, `docs/GAMESCOPE.md`): a patched gamescope composites a headless
Wayland/Xwayland session into a shared dmabuf and hands the frames back, so a Windows demo is an
ordinary picture source and gets the shaders, the grid and screenshots like everything else. What the
core cannot work out is *what to run*; `wine_command` builds the argv and `newsys/windows.rs`
restates it as core options.

```
WINEPREFIX=~/.wine-demarc wine demarc-autodlg.exe --launch demo.exe --prefer 800x600
```

**The dialog driver (`demarc-autodlg.exe`, built from `tools/autodlg`).** Nearly every PC demo opens
with a setup dialog and nobody is sitting there to answer it; the driver answers it through Win32
messages — picking the resolution demarc asked for and pressing Start/Go/Run — then starts the demo.
It **launches** the demo rather than running beside it because the driver has to be in the same
session as the dialog for `EnumWindows` to see it, which a child is and a separately-started sibling
is not.

**The driver is in the command even when there is no dialog to answer** (`wine_res=pick`, where
`--no-go` has it press nothing), because answering dialogs is only half its job. The other half is
saying **when the demo starts and when it ends**, which nothing outside the session can see: wine's
services outlive the demo inside it, so the process that was started stays alive long after the
picture is gone. The driver holds the demo's own handle and writes a line at each end; the core reads
them. Without the driver a demo still runs (someone dismisses the dialog by hand) but the session
sits there until the next entry is asked for.

**Meta keys**
- `wine_res` (`META_RES`), `WIDTHxHEIGHT`. `DEFAULT_RES = "800x600"` — the size the setup dialogs of
  the era all offer, which matters because the driver picks the mode by **matching the label** on a
  radio button or combo box entry: a size no dialog lists is a size no demo will run at.
- `wine_res=pick` (`PICK`) leaves the dialog to a person — for a demo whose dialog the driver reads
  wrongly, or one with an option only a person can decide. Input reaches the session, so it is
  answered exactly as on Windows. `--no-fill` too: the window they end up with is theirs, not a
  captured frame that has to start at the origin. A pick session runs at `PICK_RES = 1920x1200`, big
  enough to hold anything a dialog of the era offers (1600x1200 is the tallest classic mode,
  1920x1080 the widest) — whatever is picked has to fit inside the session or it comes out clipped
  (more so under `wine_desktop`, where the desktop is a hard ceiling). The session is scaled to the
  quad either way, so a demo that picks 640x480 gets a small picture in the middle: the price of
  choosing late. A pick session's size is not a choice anyone made, so it is not the one to warn
  about when it cannot be parsed.
- `wine_dll_overrides` (`META_DLL_OVERRIDES`) is `WINEDLLOVERRIDES` in **wine's own spelling**
  (`d3dx9_37=n;d3dx9_43=n`), passed through unread — no reason to invent a second syntax for
  something an entry's author already knows how to write. Whitespace-trimmed only; an empty one is
  *no* override rather than an empty variable, which to wine means "override nothing with nothing".
  Unset, `newsys/windows.rs` fills it from the DLLs the release ships beside its executable: a demo
  carrying its own `d3dx9_37.dll` needs that build and not wine's reimplementation.
- `wine_desktop` (`META_DESKTOP`) puts the pair inside a wine virtual desktop (`explorer /desktop=`)
  fixed at the session size. Demos switch display modes on their way to fullscreen, and under
  gamescope's Xwayland that means tearing down and remapping an X window, which a handful — Equinox's
  *Kings of the Playground* among them — do not survive. Inside a virtual desktop the mode switch is
  wine's own business and never reaches X. **Off by default** (`DEFAULT_DESKTOP = false`): the
  desktop is a window manager of wine's own between the demo and the screen — the picture takes an
  extra composite, the demo's fullscreen becomes a window the size of the desktop, and anything it
  does with the real display mode stops working.
- `wine_gl_compat` (`META_GL_COMPAT`) → `MESA_GL_VERSION_OVERRIDE = "4.6COMPAT"`. **Why**: a GL demo
  of the 2010s asks for a 3.x context and, as the spec allows, leaves `WGL_CONTEXT_PROFILE_MASK_ARB`
  out; the default is *core*, and a core context does not advertise `GL_ARB_multitexture`,
  `GL_EXT_draw_range_elements` or the rest of the pre-3.0 extension strings, because that
  functionality has been core for years. On Windows nobody notices — an ICD's `wglGetProcAddress` is
  a name lookup, so `glActiveTextureARB` comes back as a pointer to `glActiveTexture` whatever
  profile is current. **Wine's is stricter** and checks the extension is on the current context
  first, so the same call returns NULL — and a 64k intro, which resolves its GL entry points once
  into a table and never checks one, calls straight through it. Approximate's *Gaia Machina* dies
  exactly that way, on `glActiveTextureARB(GL_TEXTURE6)` during FBO setup. The `COMPAT` suffix is the
  operative half; `4.6` rather than the requested `3.3` so nothing else is taken away (the version is
  a ceiling, and lowering it would be a second change nobody asked for). Off by default: it is
  Mesa-only, it makes every context on the demo's side a compatibility one, and the demos that need
  it are a minority worth naming one at a time.
- `wine_sandbox` — see `wine_sandbox.rs`.

`PREFIX_DIR = ".wine-demarc"`, **deliberately not `~/.wine`**: a demo is free to install fonts,
codecs and DLL overrides, and none of that belongs in the prefix the user runs their own programs
from. wine creates it on first use.

`DIALOG_TIMEOUT = 20 s` — generous, because a cold wine prefix spends a while building itself before
the first window. `DEFAULT_CHECK = "Fullscreen"` is ticked on any demo that offers it: it saves a
Windows title bar across the top of the picture and costs nothing, since the session is already the
size of the mode. The exe path is passed as an **absolute** Unix path (wine takes those fine, but the
demo is started from its own directory, not demarc's). No shell is involved anywhere, so nothing is
quoted and nothing may be re-split on spaces — demo filenames are full of spaces, brackets and
apostrophes.

### Shutting the prefix down

`close_prefix` runs `wineserver -k`, which kills every process in the prefix. **Why it is needed**:
wine's service processes — `wineserver`, `services.exe`, `winedevice.exe`, `explorer.exe`,
`rpcss.exe` — put themselves in sessions of their own, so killing the process tree a demo was started
in does not reach them and they run for the life of the prefix. *(Measured: `gamescope -- wine cmd /c
exit` against a cold prefix was still running twenty-five seconds later with one `winedevice.exe`
left under its reaper.)* Safe wholesale because the prefix is demarc's own. What it rules out is two
demos sharing that prefix, since closing it for one closes it for both — a sandboxed session has a
prefix nobody else is in and never comes here, which is what lets several run at once.
`CLOSE_TIMEOUT = 1 s`: this runs on the way out of demarc and a wineserver that won't answer must not
hold the quit up; past the timeout it is left to finish on its own rather than swept away mid-work. A
failure is only ever "there was no server", which is the state this wanted anyway.

`sweep_prefix` kills what `wineserver -k` could not reach. A wine process outlives its own wineserver
now and then — a wedged `winedevice.exe` is the recurring one — and once the server is gone there is
nobody to ask: `wineserver -k` finds no server, says nothing, and the orphan stays until reboot.
*(Observed: thirty-seven of them left by earlier sessions with not one wineserver still running;
reproducible by ending a demo inside a session and closing the prefix afterwards.)* They are matched
the one way that still identifies them: **the prefix in their environment, compared whole**.

## `wine_sandbox.rs`

One throwaway wine prefix per demo, made with bubblewrap. **Why one shared prefix limited demarc to
one Windows demo at a time — two separate reasons:**

1. **One wineserver.** wine names its server socket after the *device and inode* of the prefix
   (`/tmp/.wine-<uid>/server-<dev>-<ino>`), so everything pointed at the same directory joins the
   same server. `wineserver -k` is then all-or-nothing: closing one demo closes every demo. That is
   what `wine::close_prefix` does, what the gamescope core's `StopWineServer` does, and why
   `docs/GAMESCOPE.md` lists "two Windows demos at once is out".
2. **One set of files.** A demo may write to the prefix — registry keys, a config in `drive_c`, a
   font it installs — and two doing it at once write over each other. The provisioning in
   `just wine-prefix` (DXVK, native `d3dx9`, registry tweaks) is also there to be preserved, and
   every demo that runs is another chance to damage it.

The fix gives each session its own prefix **without copying one**: `bwrap` mounts an overlay whose
lower layer is the real prefix and whose upper layer is an invisible tmpfs, at a path nothing else
uses.

```
bwrap --dev-bind / /                       # the host, as it is
      --proc /proc --unshare-pid           # a pid namespace of its own
      --die-with-parent
      --perms 0700 --tmpfs /tmp/.wine-1000 # wine's socket directory, private
      --overlay-src ~/.wine-demarc
      --tmp-overlay /run/user/1000/demarc-wine-1000/4711/0
      --setenv WINEPREFIX /run/user/1000/demarc-wine-1000/4711/0
      -- wine demarc-autodlg.exe --launch demo.exe ...
```

Reads come through from the real prefix so the session starts fully provisioned; writes land in the
tmpfs and vanish with the sandbox. The **private `/tmp/.wine-<uid>` is what actually guarantees a
private wineserver** — the overlay's device number would very likely differ too, but "very likely" is
not a thing to hang process isolation on. It is `0700` because wineserver refuses a socket directory
others can read.

**The pid namespace is the other half and worth as much as the prefix.** wine's services call
`setsid` and leave the process group, which is why both backends carry code to hunt them down
afterwards (`wine::sweep_prefix` exists because thirty-seven of them had piled up). Inside a pid
namespace there is nowhere to escape to: when the demo (pid 1 in there) exits the kernel takes the
rest of the namespace with it, and so does `--die-with-parent` if the session is killed from outside.
Nothing is left to sweep, and nothing that ends one demo can reach another.

**This is not a security boundary and is not meant as one**: `--dev-bind / /` hands the demo the whole
host filesystem, because it needs the GPU nodes, the audio socket, the X socket gamescope just made,
and the release's own directory. It is a *containment* device — the writes and the processes are what
is contained.

Argument-order gotchas: `--proc` must come **after** the bind, which brought the host's own `/proc`
with it — a pid namespace with the wrong `/proc` in it is worse than none. `WINEPREFIX` is set inside
the sandbox as well as by whoever spawns it, so the sandbox describes itself and the two cannot
disagree. The working directory is stated outright even though bwrap usually keeps it, because a
release that ships a `data/` folder or its own `fmod.dll` finds neither from anywhere else, and this
survives being spawned from a core that chose its own.

**Capability detection is by trying it** (`usable`): neither `bwrap` being installed nor the kernel
having unprivileged user namespaces is a safe assumption — Ubuntu's AppArmor turns the second off by
default, and an overlay inside a user namespace is newer still. Guessing from versions would be worse
than the real thing, which is one process spawn on the first Windows demo of a run: the whole
argument list with `true` in place of the demo, and the prefix overlaid onto itself (nothing is
written, and the mount goes with the process).

`prepare` **fails rather than falling back**, so the caller decides what running without a sandbox
means for it — which is not the same answer in both backends. With no prefix yet (a first run, before
wine has built one) it bails: sandboxing would build a prefix inside a tmpfs and throw it away again,
paying `wineboot` every time and keeping nothing, so the first session runs unsandboxed and creates
it for all the rest.

`META_SANDBOX` is **on by default** (`DEFAULT_SANDBOX = true`), which is what makes a grid of Windows
demos possible at all. `wine_sandbox=false` puts the session back in the real `~/.wine-demarc` — for
provisioning a release that wants to *keep* what it installs, and for a machine where `bwrap` cannot
run.

Mount points live under `$XDG_RUNTIME_DIR/demarc-wine-<uid>/<pid>/<seq>` — they are only ever empty
directories on this side (the overlay exists in the sandbox's mount namespace and nowhere else), so a
tmpfs is exactly the right place. Split by pid so `sweep` can throw away a dead run's directories
whole without deciding anything about live ones; a directory named after a live pid is left alone.
A crash leaves them behind, so sweeping is tidiness rather than repair — but an unbounded pile under
`$XDG_RUNTIME_DIR` is still a pile.

## `music_emu.rs`

Music backend for the chiptune/tracker formats `musix` handles (SID, MOD/XM/S3M, SNDH, NSF, GBS, SPC,
PSF, AHX, TFMX, …). It implements `Backend`, so a bare music file slots into the same plumbing as a
libretro core; each frame also draws a picture of the very samples it just rendered, because a music
file has no video of its own and a black window looks like a failed load. The picture belongs to a
Luau script (`music_vis.rs`); this module keeps the audio work including the delay line.

**No worker thread**, unlike the libretro and Flash backends: a frame of chip audio costs
microseconds, so it is generated inline on the frontend's thread, driven by the same `FRAME_RATE`
pacing everything else gets. Reached through `newsys::music::MusicSystem`, the last system tried —
`musix` recognises a lot of files, so anything a real machine can run is claimed first.

### The constants that encode real bugs

- **`CHUNK = 8192` samples, not one frame's worth.** The UADE plugin (AHX, TFMX, Hippel, FC — most of
  the Amiga formats) answers a request smaller than its own message size with **nothing at all,
  forever**, so asking for one 60 Hz frame (1470 samples at 44.1 kHz) yields silence. Reading in
  blocks and handing out frames from the block keeps every plugin happy; 8192 samples is ~93 ms of
  stereo, well past the threshold and still small enough not to matter for latency.
- **`EMPTY_READS_UNTIL_END = 32`.** UADE's Amiga emulation returns nothing for the first few reads
  while it boots the replayer, so the first empty read cannot be taken at face value. At one attempt
  per frame this is ~0.5 s.
- **`SCOPE_DELAY = 0.14 s`.** A frame's samples are not heard when `run` produces them: they queue in
  the frontend's ring buffer (which the frame dup/drop pacing lets float around 4500 stereo frames,
  ~95 ms) and then in the output device's buffer (2048 frames, ~45 ms). Drawing them immediately puts
  the trace that far ahead of what is playing — unmistakable on anything with a beat, an MP3 above
  all. So the scope keeps samples around and draws the ones coming out *now*. A fixed figure because
  the backend cannot see the sink; it matches the local audio path, and a sink with latency of its
  own (Bluetooth, +100–300 ms) needs it raised. **This is the knob for that.**
- `MAX_QUEUED_AUDIO = 96000` (~1 s stereo): nothing normally accumulates because the frontend
  collects after every `run`, but `--speed-test` steps the core as fast as it can and never reads
  back, so the oldest is dropped rather than growing forever.
- `FRAME_RATE = 60.0` — nothing in the formats is tied to it; it only sets how much audio is rendered
  per call and how often the scope is redrawn. `WIDTH/HEIGHT` are 4:3 to match the machines this
  music came from, and small enough that clearing and redrawing every frame costs nothing next to
  the audio.
- `MAX_SCAN_DEPTH = 4` when looking for a song inside a directory — an unpacked release is a few
  levels at most; the limit is really there so a symlink loop cannot walk forever.

### Details

`init_musix` runs once per process and **caches its result**, because `musix::init` registers a
process-global plugin list: calling it again is a no-op upstream, so a failure would otherwise be
reported only for the first song and silently swallowed for the rest. A missing data dir is a
warning, not fatal — the self-contained plugins (SID, MOD, NSF) play fine; only the data-driven ones
(UADE, sc68, AdLib) need it.

`playable_file` picks the first file `musix` can play in **lowercased** name order, top level
preferred over subdirectories — lowercased so the pick doesn't depend on the platform's idea of case
order, since the same archive must yield the same song everywhere. It requires `init_musix` to have
run: with no plugins registered every file looks unplayable.

**`to_latin1`** re-encodes metadata as ISO-8859-1, the character set the bitmap fonts a script draws
with are indexed by (`music_vis::Font::row`): text is drawn a byte at a time, so a UTF-8 `String`
would put two pieces of mojibake on screen for every accented character in a title. Anything Latin-1
can't hold (CJK, emoji, curly quotes and long dashes from a title copied out of a web page) becomes
`?`, as do control codes. Most of this metadata was Latin-1 to begin with.

`META_KEYS` are snapshotted because `musix` only offers them through a `&mut self` call, which a
`'static` Lua closure cannot hold. `meta_dirty` avoids a dozen string allocations at 60 Hz — metadata
changes a handful of times a song. Subsong changes and streamed titles arrive as meta updates while
playing, and **draining that queue is not optional** — left unread it grows for the whole playback.

**Stereo invariants.** A mono player's samples are doubled into stereo in `fill_pending` (expanded in
place back to front: sample `i` moves to `2i`, which for every sample but the first is past the ones
still waiting to move), so everything downstream only ever deals with interleaved stereo. Counts are
always rounded to whole pairs — a partial pair would swap the channels for the rest of the song, and
a short read at the end can leave an odd number. `sample_debt` carries the fractional sample between
frames so a rate that isn't a whole multiple of `FRAME_RATE` (44100/60 = 735 exactly, but 48000/50 is
not) neither drifts ahead nor falls behind.

`fill_pending` gives up as soon as a read comes back empty, so a stopped (or not-yet-started) player
costs one read per frame rather than a spin; already-handed-out samples are dropped before growing
the buffer so it stays a chunk or two long instead of the whole song.

The scope history starts **pre-filled with silence** (`resize(delay_samples(), 0)`), so a song opens
on silence rather than running ahead of the audio until the history fills.

`draw_frame` gathers everything the script reads **before** the call, so the Lua closures need no
access to `self` (which they could not outlive). Samples are divided by 32768 rather than `i16::MAX`
so the scale is exact for the common case (`i16::MIN` maps to just past -1.0). With no script, or
once one has failed, the frame goes **blank rather than keeping the last good frame**: a script
erroring every frame should look broken, not look like a paused song. `vis_failed` makes it say so
once instead of sixty times a second.

**Focus**: only the focused song renders at all — the chip emulation is the whole cost of this
backend, and unfocused views would pay it for audio that never reaches a sink. But only once there
*is* a picture, or an unfocused view that never drew a frame would show as a black tile in the grid;
the early frames of a song produce no samples (UADE boots the Amiga replayer first), so the condition
is "until one frame comes out" rather than a single attempt, and a song that ends without producing
any gives up too.

When no audio comes out in a frame the **last frame stays on screen** — a sudden blank would read as
a decoding fault — and the frontend is told about the end through `is_idle`.

`skip_frames` renders and discards whole frames: cheap for chip formats, and the only way to skip,
since seeking is optional in `musix` and most plugins don't implement it. The delay line is cleared
afterwards, because the skipped audio never reaches the speakers.

A player reporting no rate would make the frame length zero and the song would never advance, so it
falls back to 44.1 kHz — what every musix plugin resamples to by default.

**`unsafe impl Send + Sync` — read before touching.** Every player `musix::load_song` can return
(`ChipPlayer`, wrapping an opaque C++ player, and `FlacPlayer`) is declared `Send + Sync` upstream;
only the `Box<dyn MusixPlayer>` erasure loses that, since the trait carries no bounds. The
`Visualizer` is the one field that isn't a plain buffer: its `mlua::Lua` is `Send` (that is what the
crate's `send` feature buys) but **never `Sync`**, so the `Sync` impl is only sound because nothing
reaches the interpreter except through `&mut self` (`run`, `skip_frames`, `reset`). A shared
`&MusicEmu` genuinely can exist on two threads — `Emulator::core` is read through `&Emulator` in
`speed_test` while `run_retro` holds it mutably elsewhere — but none of the `&self` methods
(`with_frame`, `frame_hash`, `get_frame_size`, …) touch Lua. **Keep it that way.**

A missing/broken script is not a load failure — the song plays and the window stays blank, the same
call `init_musix` makes about a missing data directory. Only the song itself failing is worth
refusing over.

The test module generates a minimal 4-channel ProTracker module in code (one instrument with a loud
square wave, played on channel 1) so the test is self-contained — no binary asset and nothing
borrowed from the `musix` checkout. Pattern layout: 64 rows × 4 channels × 4 bytes; the sample number
is split across two nibbles, the high one sharing a byte with the period's top bits and the low one
sharing a byte with the effect.

## `music_vis.rs`

The Luau script that draws the music backend's picture (`system/lua/scope.lua`), replacing what used
to be a hard-coded oscilloscope. The backend keeps the audio work — most importantly the delay line
that makes the trace line up with the speakers — and hands the result to a script that owns every
pixel.

**Luau rather than PUC-Lua for its native `buffer` type**: the script draws into a byte-addressed
block with `buffer.writeu32` instead of crossing the FFI once per pixel.

**The colour packing contract.** The frontend wants each pixel's *memory* bytes to be `[r, g, b, a]`;
Luau's `buffer.writeu32` is little-endian. The `rgb()` global packs so `writeu32` lays those four
bytes down in that order, and `Visualizer::render` reads each group of four back as a **native-order
`u32`** — which is exactly the packing the frontend wants, on either endianness. **A script that
builds `0xAARRGGBB` by hand instead will have its channels swapped.** Colour components are masked
rather than range-checked, and taken as signed: a script deriving a colour from a waveform will hand
over a negative or an overshoot sooner or later, and erroring there would blank the frame over one
bad pixel.

**Everything the script can ask about lives in `VisData` behind a mutex**, because the Rust closures
registered as Lua globals outlive any one call and (with mlua's `send` feature) must be `Send`; they
own a handle to the shared data rather than borrowing the backend, which they could not outlive. A
poisoned lock would mean a Lua closure panicked mid-call; nothing is left half-written by that, so it
carries on rather than taking the song down.

`samples` is the **delayed** audio — what is coming out of the speakers now, not what was just
rendered. `meta` values are ISO-8859-1 bytes, not UTF-8, because that is the character set
`Font::row` indexes by.

### Spectrum

`FFT_SIZE = 1024` — at 44.1 kHz that is ~23 ms, a shade under two 60 Hz frames: long enough to
resolve the bass notes a spectrum display is mostly about, short enough to still look responsive.
`frame_count` doubles as the cache key so two `get_spectrum()` calls in one frame do one transform.
Bins are **log-spaced**, because a linear split of the 513 raw bins puts every note a chip tune
actually plays into the first handful, leaving most of a spectrum display permanently flat. Bin 0 is
DC and is skipped. Scale is `4.0 / FFT_SIZE`: `2/N` turns a bin magnitude into the amplitude of the
sinusoid that produced it, and the other factor of 2 undoes the Hann window's coherent gain of 0.5,
so a full-scale tone lands near 1.0. Early in a song there are fewer than `FFT_SIZE` pairs, so the
window opens on leading silence rather than on whatever the uninitialised tail held.
`process_with_scratch` uses `input` as scratch too, so its contents are rubbish afterwards — fine,
it is rewritten every call.

### Reload

The `Script` is recreated wholesale on reload, so nothing the previous script left in a global leaks
into the next. A reload that fails to compile is **logged and the previous script kept**: half-saved
files are normal to observe while someone is editing, and the last working visualization beats a
black screen.

**`watch` watches the *directory*, not the file.** Editors overwhelmingly save by writing a temporary
file and renaming it over the target, which replaces the inode and leaves a watch on the file itself
pointing at the old one — the first save would be seen and no other. Filtering the directory's events
by file name survives that. No debouncing: a burst of events for one save collapses into a single
boolean and the reload happens once, on the next frame.

The Lua state is built with an explicit library set. **Luau has no `io` or `package` to begin with;
naming the set we do want keeps it that way if that ever changes.** `os` is in for `os.clock`. An
optional `Init` function lets a script precompute tables before frame one; `render` is looked up once
rather than per frame.

### `noise([...])`

With no arguments it draws from a stream (sparks, dust, jitter). **With numbers it is a hash of
them**: the same arguments always give the same result, which is what a script wants for anything
that must stay put frame to frame (the brightness of a star at `noise(x, y)`, the phase of a bar at
`noise(bin)`) and which a stream cannot give without storing a number per thing. Luau's
`math.random` is registered too and is the better tool for a plain die roll.

SplitMix64's finalizer is used for the mixing — enough avalanche that neighbouring pixel coordinates
give unrelated values, which is the whole point of hashing rather than scaling them. Arguments are
**folded, not summed**, so `noise(1, 2)` and `noise(2, 1)` differ, and `+ 0.0` so a coordinate that
came out as `-0.0` hashes like the `0.0` it equals. The result takes the top 53 bits (what a double
holds exactly), giving every representable value in `[0, 1)` with equal probability. The stream's
seed is arbitrary but fixed, so a script that looks right once looks right again.

### Fonts

A bitmap font in the headerless "raw" format Amiga font packs ship in: 256 glyphs back to back, one
byte per pixel row, MSB leftmost, so glyph `c` starts at byte `c * height`. **Nothing in the file
says how tall a glyph is — there is no header at all — so the height comes from the length.** The
Topaz files are 4096 bytes, i.e. 8x16: the 8x8 Amiga font with every row doubled, which is what gives
it the right proportions on a square-pixel display. `FONT_WIDTH = 8` always: one byte, one row.

Fonts are **embedded, not read from disk**: the Lua state deliberately has no `io`, and letting a
script name a path would be the one hole in that. It also means a visualization draws text whether or
not the system directory survived being moved. See `src/fonts/README` for provenance. The character
set is whatever the font file uses — ISO-8859-1 for these — so a Lua string is indexed byte by byte
and a script handing over UTF-8 gets mojibake rather than an error. The font handle is `Copy` (a
slice reference), so a script may keep one in a global and hand it to every `text` call, and exposes
`width`/`height` so it can lay out lines without hard-coding the size of a font it asked for by name.

### Drawing primitives

A script *can* do all of this with `buffer.writeu32`, and for scattered pixels it should. These exist
for filled areas: a faithful oscilloscope draws a vertical run per column and a spectrum draws a bar
per bin, together up to `WIDTH * HEIGHT` writes a frame, **each one an FFI crossing plus a bounds
check**. Filling a scratch row in Rust and handing it over one `write_bytes` per rectangle row turns
the whole frame into a few hundred crossings instead of a few hundred thousand. One scratch buffer is
shared so a frame's drawing reuses one allocation.

Everything is **clipped rather than rejected**: a script computing coordinates from a waveform will
run off the edge now and then, and an error there would blank the whole frame. Text is clipped per
glyph and then per run, since a script drawing a song title has no idea how wide it is until too
late; only the set pixels are written so text lands over whatever is already there, and they come in
horizontal runs (doubled pixels make those at least two wide) with one `write_bytes` per run.

`clip` treats a **negative extent as empty rather than mirrored**: `w` is a width, and a script that
computed a negative one has a bug that silently drawing something would only hide.

## `image_emu.rs`

Static image backend: presents a decoded still image through the same plumbing as a core — one RGBA
frame via `with_frame`, geometry reported, every interactive method a no-op. Decodes ILBM/IFF
(`ilbm.rs`), DEGAS (`degas.rs`), ZX Spectrum screens (`zx_scr.rs`), palette TIFF (`tiff_pal.rs`) and
the `image` crate formats (PNG, BMP, JPEG, TGA, PCX).

**Colour cycling.** Paletted images can define cycling ranges (ILBM CRNG chunks, DEGAS Elite colour
animation, ZX Spectrum FLASH attributes). With `--color-cycle` the image is kept in **indexed** form
and the RGBA frame is regenerated in `run` from a palette rotated by elapsed frames, animating the
picture the way DeluxePaint did. Every paletted format lands in the same indexed representation, so
cycling works identically across all of them. `CRNG_RATE_60HZ = 16384.0` is the CRNG `rate` value
corresponding to 60 cycle steps per second. Only ranges that actually animate **and** stay within the
palette are kept; anything else is left as a fixed colour, and with cycling disabled the list is
empty and the frame is static. `reverse` just flips which way colours travel. Time is derived from
the frame count on the assumption that the frontend calls `run` at `FRAME_RATE = 60.0`; **no wall
clock is consulted**. The frame is only rebuilt when the rotation has actually moved
(`last_offsets`), and `cycle_offsets` reduces modulo the range length so equal vectors mean an
identical-looking frame.

**Format sniffing order and why.**
- A **ZX screen is identified by its size alone**, which a file of another format can hit by chance,
  so unlike the others it is only decoded when the *name* says so too.
- **The ST's formats have next to no signature either.** DEGAS opens with a resolution word followed
  by a palette, which another format's first 34 bytes can pass for (a 32-bit TGA opens with two zero
  bytes, which read as a valid low-res word), and NEOchrome opens with a word of nothing at all. The
  decoders themselves only check there is enough data, so they would happily turn the head of any
  large file into a screenful of noise; the full sniff — which also weighs the palette nibbles and
  the exact file size — is what keeps them to real pictures. Gated on 11 extensions: DEGAS and
  CrackArt in each of three resolutions, plus NEOchrome and Fullscreen Construction Kit (low-res
  only).
- **Palette TIFF is here for a different reason**: the `image` crate refuses it outright, so this is
  the only decoder for it. Its signature is a real one, so it needs no help from the file's name.
- Failing all of those: the full IFF decoder (HAM, deep, dynamic-palette); failing that, the `image`
  crate.

Each decoder is paired with its own description of the file, so the one that wins names the format
`get_info` reports. The file is read **once** and the same bytes offered to every decoder.

`load_image` sniffs the format from contents rather than trusting the extension, so a mis-named file
still decodes — **except TGA, which has no signature** and falls back to the extension the reader was
opened with. The format is named while the reader still exists (decoding consumes it). Depth is the
depth of the *decoded* pixels, not the file — a paletted image arrives already expanded — and a
truecolour file that in fact uses few colours (pixel art saved as PNG, a converted screenshot) is
described by its **colour count** instead, up to `MAX_COUNTED_COLORS = 256`; `count_colors` bails as
soon as the limit is passed, since there is no reason to keep tallying a photograph's thousands.
Past that count the two truecolour depths look the same, so they are named the same.

`image_extras::register()` teaches the `image` crate to decode PCX by extension and by signature; it
is idempotent and internally `Once`-guarded, so calling it per load keeps it next to the code that
needs it.

`serial` starts at **1** because both constructors leave a rendered frame behind; the initial
unrotated frame is rendered in the constructor so the first presented frame is correct before any
time has elapsed. Pixels are packed so bytes land in `[r, g, b, a]` memory order.

## `flash_emu.rs`

Flash (SWF) backend on Ruffle, behind the `flash` feature.

Ruffle renders through `wgpu` (it has **no CPU rasterizer**) and its `Player` carries a thread-local
gc-arena, so — mirroring `RetroCoreThreaded` — the emulator lives on a dedicated worker thread owning
the `Player`, an offscreen wgpu instance and the audio mixer. `FlashEmu` itself holds only channels
plus a cached frame/audio buffer, which makes it trivially `Send + Sync` (required for the Bevy
component) and keeps all wgpu work off Bevy's main thread. `update_rx` is behind a `Mutex` only for
`Sync` (`Receiver` is `Send` but not `Sync`); `run`/`Drop` take `&mut self` so it is always accessed
via `get_mut` with no locking cost.

`SAMPLE_RATE = 44100` matches the other cores so the existing `AudioResampler` converts to the device
rate. `UPDATE_QUEUE = 3` — a few frames of slack; **blocking sends provide the backpressure** that
makes the worker run at the frontend's consumption rate.

**`run` consumes exactly one frame per call**, like `RetroCoreThreaded::run`: the frontend paces
`run()` at the movie's fps and assumes each call carries one frame's worth of audio. Draining the
whole channel instead would push several frames of audio per paced call (flooding the sink past
`AUDIO_BUF_MAX` → continuous "Dropping frame") and would keep the bounded channel from ever filling,
defeating the blocking-send backpressure.

**"One frame" is `tick(1/fps)`, not a bare `run_frame()`.** Alongside the timeline, `tick` advances
`flash.utils` timers, `NetStream`/streamed-sound playback and the audio backend — machinery
timer-driven AS3 content (a Flex/Away3D demo whose menu and music are timer/stream driven) needs to
progress at all — and it stream-preloads incrementally so big movies present as they load. Passing
the movie's own frame duration advances exactly one frame; **it is a per-frame quantum, not
wall-clock pacing.** The worker does no timing of its own. The navigator's executor is pumped each
iteration so pending external fetches make progress; results are delivered on the next `tick`.

**The navigator is rooted at the movie's own directory** so relative external loads
(`URLRequest("99er.mp3")` for streamed audio, or any sibling asset) resolve to the files next to the
SWF. Without this the default null navigator has an empty base path and every relative fetch fails,
which can leave preloader-gated movies stuck on a black frame. `NullNavigatorBackend` runs fetch
futures on its own `NullExecutor`, which the worker must pump. `parent()` yields `Some("")` for a
bare filename like `99er.swf`, which can't be canonicalized — fall back to the current directory.

**`set_allow_fullscreen(false)` is required.** This player has no OS window and renders into a
fixed-size offscreen target sized to the movie's stage. Honouring `Stage.displayState = FULL_SCREEN`
would switch the stage to a fullscreen size the movie then scales its content to — landing outside
the fixed target and presenting as a black frame. The default UI backend otherwise **accepts** the
request, so this must be set explicitly. The frontend scales the output to the display instead.

**`capture_frame_fast` — an 84% win.** Ruffle's own `capture_frame` (via `buffer_to_image`) spends
the overwhelming majority of its time in `unmultiply_alpha_rgba`, a per-pixel float divide converting
premultiplied to straight alpha. **Profiling showed that pass alone at ~84% of CPU time for typical
content (~25% for vector-heavy movies).** Frames are presented opaque, so straight vs premultiplied
alpha is irrelevant: the divide is skipped entirely and the code just de-pads the rows (the buffer is
padded to `padded_bytes_per_row`) and sets alpha to 255. It works by downcasting the player's renderer
to the wgpu backend and mapping the render target's staging buffer directly — the same technique
Ruffle's exporter uses; a handle to the descriptors is kept for that. Returns `false` if the renderer
isn't wgpu or has no readback buffer, leaving `out` untouched. The slow `capture_frame` is kept for
the rare `SavePng` path where exact colours matter.

`FlashAudio` is a thin owner of an `AudioMixer`; registration/playback is delegated by the macro and
the mixed output is pulled through an `AudioMixer::proxy` kept in the worker — exactly how Ruffle's
cpal backend works. Playback is pull-driven by the frontend, so `play`/`pause` are no-ops.

Setup runs on the worker but its `SetupResult` is awaited synchronously by `FlashEmu::new`, so SWF /
GPU load errors surface at load time. On drop, any queued frame is drained first so a blocked worker
send can unblock, then the thread is joined.

Input: the absolute pointer from the frontend is mapped into stage pixels so Ruffle's cursor lands
where the visible OS cursor is. Key handling emits a character on key-down for printable keys to
drive text fields (`RETROK_*` are ASCII-valued in the printable range). Only common game keys are
mapped in v1. A full reset would rebuild the movie and is not supported in v1.

## `ilbm.rs` — Amiga IFF/ILBM decoding

Handles ILBM, PBM (DeluxePaint PC), ACBM, and Impulse RGB8/RGBN.

**Chunk parsing.** A parsed chunk always holds exactly its header plus payload, which is what makes
the accessors infallible. **The outermost chunk is parsed *without* trusting its size field**:
real-world IFF files routinely carry a stale FORM size (both short and past the end of the file), so
the whole buffer is kept and the chunks inside are read as far as they go. Sub-chunks are parsed
strictly, so a malformed one is still rejected; a chunk that doesn't fit ends the iteration. Chunk
data is padded to an even byte boundary and **the pad byte is not counted in the size field**.

### Aspect correction (`display_scale`)

On the Amiga a lores pixel is roughly square, a hires pixel is half as wide, a super-hires one a
quarter as wide (so those images are stretched vertically), while an interlaced pixel is half as tall
(so lores-interlace images are stretched horizontally). A hires-interlace pixel is square again; a
super-hires interlace pixel is still half as wide as it is tall. Correction is by integer pixel
replication.

- Resolution comes from the CAMG viewport mode when present, but **only the HIRES/SHRES/LACE bits are
  trusted** because old writers leave garbage in the others.
- **SHRES additionally has to be backed up by the width**: the extended mode ids (Productivity,
  DblPAL and friends) set that bit on 640-pixel screens whose pixels are already square, whereas a
  genuine super-hires screen is 1280 wide (1024+ even trimmed down).
- With no CAMG, size is inferred from the dimensions (the classic 320x512 → 640x512 and 640x256 →
  640x512 cases; hires ≈ 640+ wide, interlaced ≈ 400+ tall). Super-hires is **not** guessed at — such
  a picture is Amiga-only and in practice always carries a CAMG.
- PBM (PC DeluxePaint) images always have square pixels and are left alone.
- **A BMHD declaring equal x and y aspect overrides all of that.** The ratio itself is too often left
  at a writer's stale default (10:11 on a hires screen) to be worth reading in general, but writing
  the two the same is a deliberate "these pixels are square" that some modern pictures rely on — an
  image drawn square and saved with a hires mode id would otherwise be stretched to twice its
  intended height.

### Formats and their quirks

- `unpack_byterun1` (PackBits) also returns **how many bytes it consumed**, because callers that
  store something after the packed data (see `degas.rs`) need to know where it ends. Literal run:
  copy next `n+1` bytes. Repeat run: repeat next byte `-n+1` times; `-128` is a no-op.
- ILBM rows are padded to a 16-bit boundary (`width.div_ceil(16) * 2` bytes per plane per row); a
  mask plane (`masking == 1`) is stored as an extra plane per row. Up to 8 planes → palette indices;
  24/32 planes → deep truecolour (planes 0..8 red, 8..16 green, 16..24 blue; higher planes, e.g. a
  32-plane alpha byte, ignored).
- ACBM stores bitplanes **contiguously** (all of plane 0, then all of plane 1, …) and is never
  compressed.
- PBM is chunky, one byte per pixel, rows padded to an even byte width.
- RGBN (12-bit): RLE runs of a 16-bit word `rrrr gggg bbbb g c c c` — top 12 bits are the 4-bit
  components, low 3 bits a run length (bit 3 is genlock, ignored). Components are replicated into
  both nibbles for full 0..255 range.
- RGB8 (24-bit): RLE runs of four bytes `r g b c`, `c`'s low 7 bits the run length (bit 7 genlock).
- For both, a **zero run length escapes to a byte, then to a big-endian word** (`read_run_len`).
- **Extra-HalfBrite** expands the palette to 64 registers so a plain index lookup yields the upper
  half-bright colours, letting EHB use the same indexed path as any other palette image.
- **HAM** carries colour forward across a scanline, seeded from black. The two high planes are
  control bits; the rest select or modify a component, and the data value (4 bits for HAM6, 6 for
  HAM8) is expanded to a full 8-bit component by replicating the high bits into the low ones.

### Dynamic palettes

Per-scanline palettes come from SHAM, CTBL, BEAM or PCHG. Such images cannot be shown as a single
indexed frame, so they take the fixed-RGBA path.

- CTBL/BEAM: a flat table of 12-bit words, one group per row. The registers-per-row count is taken
  from the chunk size, so 16-register images (including HAM6, which has 16 base registers) and wider
  tables both work; trailing rows past the table reuse the last row's colours.
- SHAM: a version word then 16-colour 12-bit palettes, one per (possibly doubled) scanline — each
  output row is mapped onto its palette by **scaling**, so a 256-palette SHAM over a 512-line image
  gives one palette per two rows, matching how the hardware reloads colours every other line.
- PCHG: only the **uncompressed, small (12-bit)** change format is handled; anything else
  (Huffman-compressed, or 24-bit "big" changes) returns `None` and the caller falls back to the
  static palette. A line-change bitmap follows the 20-byte header: `line_count` bits packed into
  big-endian 32-bit longwords, bit `(31 - i%32)` of longword `i/32` marking that scanline
  `start_line + i` carries a `SmallLineChanges` record — which is a count for registers 0-15, a count
  for 16-31, then that many change words (register in the top nibble, 4:4:4 RGB in the low 12 bits).
  The running palette is **padded to 32 registers** so 16..31 changes have somewhere to land even for
  shallow images.
- An empty result is dropped so a malformed chunk falls back to the static palette.

### CRNG / colour cycling

`CRNG` layout: `pad(2) rate(2) flags(2) low(1) high(1)`. `rate` is in CRNG units where **16384 == 60
steps per second**, so Hz is `rate * 60 / 16384`. Flag bit 0 = active, bit 1 = reverse.

### `palette_bits` — 4-bit (OCS/ECS) vs 8-bit (AGA) colour

A 4-bit value is written out either replicated into both nibbles (`$f` → `0xff`, what the IFF spec
asks for) **or in the high nibble alone** (`$f` → `0xf0`, what plenty of older writers do), so a map
entirely of one of those two shapes came from 4-bit hardware and anything else needs AGA. Best
effort: a genuine 8-bit palette that happens to use only such values (a black-and-white one) reads as
4-bit — which is the right answer about its colours if not about the machine that made it.

### `describe`

Reads **only the header chunks, never the BODY**, so it is cheap and describes an image whichever
decode path it goes down. The truecolour forms carry their depth in the form type (RGB8 = 24-bit,
RGBN = 12-bit); a deep ILBM's planes are colour bits rather than register bits, so 24+ of them mean
truecolour. Palette depth is only meaningful for the Amiga paletted forms — a PC PBM's palette is a
VGA one, and the truecolour forms carry colour per pixel. A dynamic-palette chunk is what makes such
a picture what it is, so the one in play is named. The size reported is the **displayed** one, since
aspect correction replicates pixels and that is what the frontend shows.

`load_indexed_from_memory` fails for HAM and truecolour, whose pixels aren't plain palette lookups.
Aspect correction there replicates **indices, not colours**, so the palette (and any cycling applied
to it) is untouched.

## `degas.rs` — Atari ST still images

DEGAS (`.PI1`, `.PC1`), NEOchrome (`.NEO`), CrackArt (`.CA1`–`.CA3`) and Fullscreen Construction Kit
(`.KID`). DEGAS is the ST's answer to ILBM but with **none of the chunk structure**: a file is a raw
dump of a screen mode — two bytes of resolution, sixteen palette words and 32000 bytes of screen
memory (`SCREEN_BYTES`, the size of an ST framebuffer **in every resolution**; fewer bitplanes buy
proportionally more pixels), optionally followed by DEGAS Elite's 32-byte colour-animation trailer.

**`.PC1` reorders as well as compresses.** An uncompressed file interleaves the bitplanes word by
word, the way the shifter fetches them; a compressed one stores each scanline **one whole plane at a
time**. (`Layout::Interleaved` vs `Layout::Sequential`; NEOchrome and CrackArt are interleaved.)

- NEOchrome pads the header to 128 bytes with the picture's original filename and one channel of
  colour animation, then stores the screen untouched. Its resolution word *can* say what DEGAS' does,
  but the program only ever drew in low resolution.
- CrackArt is the only one with a signature, and the only one whose compression beats PackBits.
- `.KID` stores an **overscanned** screen — borders opened, 448x274, scanlines 230 bytes where a
  low-res framebuffer's are 160, and 274 of them rather than 200. It is the only picture here that is
  not 32000 bytes, and every `.KID` has exactly one size. Six bytes per line lie beyond the pixels
  this decodes.

Everything decodes into the same `IndexedImage` the ILBM decoder produces, so `ImageEmu` animates a
DEGAS Elite or NEOchrome colour animation exactly like a DeluxePaint CRNG one. All three screen modes
decode (they differ only in how many planes the 32000 bytes are split into), though what a demo ships
is almost always low resolution. Medium-res pixels are half as wide as low-res, so the picture is
doubled vertically to square them up; monochrome is already square.

### The ST/STE palette word

A colour word is `0000 rrrr gggg bbbb`. **The original ST only had three bits per component, in bits
2-0 of each nibble; the STE added a fourth bit that is *less* significant than those, in bit 3**,
making an STE component `(n & 7) << 1 | n >> 3`. Reading an ST picture that way would darken it (its
white would come out at 238), so the two are told apart by **whether any component uses bit 3 at
all**. For 3-bit values the spread is `(n << 5) | (n << 2) | (n >> 1)`, so 7 → 255.

CrackArt stores only the registers the mode can show, and **none at all in monochrome** — the ST
shows register 0 as white, so a fixed white/black palette is used.

### Colour animation

**DEGAS Elite trailer**: four parallel arrays of one word per channel — first and last register, the
direction (0 left, 1 off, 2 right) and a delay, where `128 - delay` (`MAX_ANIM_DELAY`) is the number
of vertical blanks between steps, so the word counts *down* and 0 is the slowest setting. Channels
switched off, or never set up (all zeroes), describe an empty range and are dropped. CRNG states a
speed where DEGAS states a delay, so the conversion is `16384 / vblanks`.

**NEOchrome** has one channel in two words: the limits word holds first and last register in the
nibbles of its low byte and its **top bit says whether the animation was ever set up**; the speed
word's top bit is what switches it on, and its low byte read **signed** is one more than the number
of vertical blanks between steps, negative for a leftwards cycle. A count of zero is out of that
reckoning and taken as the fastest step there is — one every vertical blank.

CrackArt and KID have nowhere to put colour animation.

### CrackArt's compression (`unpack_crackart`)

The stream opens with the escape byte, a fill byte and the offset word (whose **top bit is not part
of the offset**), then RLE with two twists:

1. **The offset.** Consecutive bytes are not stored consecutively but that far apart, starting again
   one byte further along each time the walk runs off the bottom of the screen. Written a scanline at
   a time, as every file in the wild is, this walks **down** the screen a column at a time — where a
   picture repeats itself far more often than it does along a scanline. Whatever the walk overshot by
   is dropped.
2. **The fill byte**, which the whole screen starts out holding, so a run of it need not be stored —
   only stepped over, and the run that reaches the end of the picture not even that. A fill run's
   count is a word whose high byte is never zero: **a zero there means the fill runs to the end of
   the picture**, which the screen is already full of.

Control bytes: the escape byte itself when it appears in the picture; `0` = a run counted by a byte,
then one counted by a word; `2` = a fill run; anything else = the short form, count in the control
byte with the byte to repeat following.

A stream that ends early leaves the rest of the screen as the fill byte — **half a picture beats
none**.

### Sniffing (`is_st_image`, `SNIFF_BYTES = 36`)

`data` may be a prefix of the file with `len` its full length.

- **DEGAS** has no signature, so: the resolution word, the size an uncompressed file is obliged to
  have, and that every palette word leaves its top nibble clear the way an ST colour does. For the
  compressed variant (bit 15 of the resolution word) the packed size is anyone's guess, but it must
  be smaller than a plain screen — **packing that expands a screen means the writer would have stored
  it plain**.
- **NEOchrome** opens with a word of nothing at all, so there is little beyond the palette and the
  one size the format has: header plus screen, never compressed, never with a trailer.
- **CrackArt** has a signature, leaving the size and the palette the resolution says is there.
- **KID** has a signature and is never compressed, so there is exactly one size it can have.

`format_of` (used once a file is *believed* to be an ST picture) is deliberately looser: only the
four have to be told apart. CrackArt and KID have magics; NEOchrome and an uncompressed DEGAS both
open with a zero word and are separated by size, which each is obliged to have exactly.

`describe` reads only the resolution, so it costs nothing, and names all four the same ("Atari") —
**which program wrote the file says nothing about the picture**. Aspect correction replicates
indices, not colours, so the palette and any cycling are untouched.

## `tiff_pal.rs` — palette-colour TIFF

**The one still format the `image` crate refuses.** The `tiff` crate under it errors out of
`colortype()` for `RGBPalette` *before any pixels are read*, and `ColorMap` is an unread tag there.
So the whole file is parsed here — but **only the palette case**. Everything else in a TIFF
(truecolour, greyscale, CMYK, tiles, JPEG-in-TIFF) is left to the `image` crate, which decodes it and
decodes it better; this decoder bails and `ImageEmu` falls through.

Decoding into the same `IndexedImage` the ILBM/DEGAS decoders produce is the **point** of doing it
here rather than expanding to RGBA: the palette survives, so a paletted TIFF is a real paletted
picture in the frontend. TIFF has no equivalent of a CRNG chunk, so nothing cycles by itself, but the
representation matches.

**Supported**: 1/2/4/8 bpp, either byte order, striped images with no compression, PackBits or LZW,
and horizontal differencing. That covers what picture converters actually write; anything else is an
error naming what it ran into. PackBits is the same run-length coding as an IFF BODY's ByteRun1, so
`ilbm::unpack_byterun1` decodes it unchanged.

Deliberate refusals, each with a reason:
- **BigTIFF** (magic 43): 64-bit offsets and a different directory layout, needing its own reader. No
  picture converter writes one.
- **Reversed fill order** (first pixel in a byte's low bits): legal and essentially unused; refusing
  beats decoding it mirrored.
- **Tiles**: replace strips with a grid of rectangles and bring their own tags. Converters write
  strips.
- Multi-page: later pages ignored — a picture file has one.
- Horizontal differencing is only accepted at 8 bpp: differences are taken between *samples*, so they
  only line up with bytes at the one depth where a sample is a byte.

`MAX_DIMENSION = 1 << 16` — TIFF states dimensions in 32-bit fields, so without a bound a corrupt
header would have us allocate gigabytes before reading a strip.

Structure notes: a directory entry is 12 bytes (tag, type, value count, then four bytes that are
either the values or an offset to them) — `Entry::at` is resolved to an absolute offset either way so
readers need not care which. An index is a single sample by definition, which also settles
`PlanarConfiguration`: with one sample its two values describe the same layout, so the tag isn't
worth reading. `RowsPerStrip` left out means the whole image is one strip; it is capped at the height
so the arithmetic stays in range (the tag's default is `2^32 - 1`) without changing what it means.
Every read of the file goes through `read_field`, so a truncated or lying file is a message rather
than a panic.

**`ColorMap` stores its channels as three consecutive runs**, not interleaved: every red, then every
green, then every blue. Channels are specified as full-range 16-bit values so the top byte is the
colour — but **some writers put an 8-bit value in the 16-bit field**, which would decode as an
all-black palette, so a map that never exceeds 255 is taken at face value (no real 16-bit palette is
that uniformly dark).

**LZW.** Codes are packed **most significant bit first** (the opposite of GIF), start at 9 bits and
grow by one each time the table fills, up to 12. The two reserved codes hold no string; empty entries
stand in for them so a string's code stays its index. One code may be used before it is defined — the
encoder emits it for a run whose string it has just added, which is the previous string plus its own
first byte. Past the widest code there is no room for another string and the encoder owes us a clear
code, so the table stops growing.

> **TIFF's LZW grows the code width one code *early*.** A decoder that waits for the table to be
> genuinely full reads every later code shifted — the classic way to get garbage out of an otherwise
> correct implementation. At 511 entries the next code the encoder writes is already ten bits wide.

## `zx_scr.rs` — ZX Spectrum screen dumps

**The Spectrum has no picture format of its own**: a `.SCR` is a byte-for-byte copy of the 6912 bytes
of video RAM — no header, no palette, no version. **The only thing that identifies it is its size.**

Video RAM is two halves: 6144 bytes of bitmap (one bit per pixel, 32 bytes per row) then 768
attribute bytes, one per 8x8 cell, each naming the two colours that cell's set and clear bits take.
Colour therefore has a coarser resolution than the bitmap, and the palette is fixed: eight colours at
two brightness levels.

**The display file's address scrambling** (`row_offset`): the address is
`010 t2t1 p2p1p0 r2r1r0 c4..c0` — the screen's third, then the pixel row *within* a character cell,
and only then the cell row. Consecutive addresses walk the eight rows of a cell only every 256 bytes,
which is why an unscrambled read of a `.SCR` comes out as eight interleaved combs. The offset is
`((y & 0xc0) << 5) | ((y & 0x07) << 8) | ((y & 0x38) << 2)`.

**Palette register layout**: the three colour bits are wired straight to the ULA's outputs, so a
register is `bright:green:red:blue` with blue at the bottom — which is why the Spectrum palette runs
black, blue, red, magenta rather than through red first. `NORMAL_LEVEL = 0xd7`: a non-BRIGHT
component stops a little short of full, because the ULA drives the same pin without the brightness
line pulling it all the way up. BRIGHT applies to the whole cell, so it becomes the top bit of both
ink and paper registers.

**FLASH as colour cycling.** The ROM's interrupt routine exchanges a flashing cell's two colours
twice a second — a palette rotation over two registers — so it is expressed as a `CycleRange` and
animated by the same code that runs ILBM CRNG cycling or a DEGAS Elite animation. But **cells that
flash cannot use the fixed sixteen registers**, since a rotation applied there would flash every cell
sharing those colours: each flashing combination *present in the picture* gets a private pair of
registers with a two-entry range over them, so the register budget is spent on cells really on
screen. Swapping is skipped when ink and paper are the same (invisible). Which way a two-entry range
turns makes no difference. `FLASH_TOGGLE_FRAMES = 16` of the ROM's 50 Hz frames, so a full cycle
takes just under two thirds of a second, restated in CRNG units.

**A bitmap on its own is a legitimate dump** — the two halves live in different places in a
Spectrum's memory map, and tools that only wanted the picture saved only the first; those are shown
with `CLS_ATTR = 0x07` (white ink on black paper), what the ROM leaves attributes at after a `CLS`.

`is_screen(len)` accepts only the full 6912 bytes or a bare bitmap: **anything looser would claim
files of other formats that happen to be big enough**, and with no header there is nothing else to
check. `describe()` takes no arguments — every screen is described the same way, since there is
nothing to read out of the file.

## C / C++ shims

### `adf_unpack_shim.c`

Unpacks an ADF disk image into a host directory, serving `--unadf`.

**Why the walk is C rather than Rust**: ADFlib's directory cursor is a field of `struct AdfVolume`
(`curDirPtr`, moved by `adfChangeDir`/`adfParentDir`) and `adfFileOpen` resolves names against it.
Driving that from Rust would mean duplicating ADFlib's struct layouts in a bindings file that no
compiler checks against the headers; here they come from the headers themselves. Modelled on
`extract_tree()` in ADFlib's own `examples/unadf.c`. The image is opened read-only and never written
back.

**`safe_name` refuses rather than sanitises.** Names come from the image, so they are
attacker-controlled as far as this code is concerned, and they are about to be pasted onto a path.
AmigaDOS allows almost every byte in a file name, including the ones that would make this an escape
from the destination directory. Rather than rewrite those into something that merely *looks* safe,
the entry is refused — a demo disk holding a file called `..` is not one we want to boot anyway.
Refused: `..`-style climbs, both kinds of path separator (the drive is also read on Windows), the
AmigaDOS volume separator, and controls including NUL padding.

**The bytes that are kept are transcoded ISO 8859-1 → UTF-8**, because that is the boundary both
Amiga cores draw: an Amiga file name is Latin-1, a host file name is UTF-8, and each converts on
every call into the file system. amiberry's `my_readdir` runs the host name through
`utf8_to_latin1_string` and **skips the entry** when that fails; puae's runs it through
`utf8_to_local_string_alloc`. So a name written out as raw Latin-1 — `3d-demo.adf` ships one,
`Har vi r\xf8get hash?` — is not a file the emulated Amiga can see at all. Writing the same bytes the
cores would write is what makes it visible. (Latin-1 only ever reaches U+00FF, so two bytes always
suffice; buffers are sized at twice the longest AmigaDOS name plus a terminator.)

Bounds and refusals: a directory tree deeper than the limit is a corrupt image, not a demo. An 880K
floppy cannot hold more than 880K of file data, so anything past a few megabytes means the block
chains are looping and `adfFileRead` will never reach EOF — bounded so a bad image fails instead of
filling the disk. Hard and soft links are skipped: they point outside the entry they sit in, and
following them is how a walk ends up in a loop or outside `dest`. A file the image cannot produce is
**skipped rather than failing the whole unpack** — half a release is still worth booting, and the
caller checks for a startup-sequence anyway.

ADFlib reports unreadable blocks and bad checksums straight to stderr, and being handed a non-DOS
demo disk is an **expected** outcome here, so the log callbacks are silenced. `demarc_adf_init` is
called once per process (`adfAddDeviceDriver` appends to a global list) and is never paired with
`adfLibCleanUp`. Not re-entrant — ADFlib keeps its environment in globals, so the Rust side holds a
mutex across it.

### `dms_unpack_shim.c`

Turns a DMS archive into the ADF inside it, for `--unadf`: both Amiga cores read `.dms` straight off
the disk, but ADFlib does not. The unpacker is xDMS 1.3 by way of amiberry (`external/dms`); all this
adds is opening the two files and giving Rust a stable set of error codes. The output is a normal
`.adf`: 80 (or 160 for HD) tracks of 11264 bytes **written at their own offsets**, so a DMS missing
tracks still yields an image with the tracks it does have in the right places. Flush before measuring
the size — the last track is still in the stdio buffer.

### `unrar_isnt_shim.cpp`

Stand-in for unrar's `isnt.cpp`, needed **only when cross-compiling to Windows**. `unrar_sys`' build
script selects its source list with `cfg!(windows)`, which in a build script describes the *host*,
not the target — so cross-compiling from Linux drops `vendor/unrar/isnt.cpp` while the rest of unrar
still calls into it:

```
lld-link: error: undefined symbol: unsigned long __cdecl WinNT(void)
lld-link: error: undefined symbol: bool __cdecl IsWindows11OrGreater(void)
```

The signatures match `vendor/unrar/isnt.hpp` exactly so the MSVC-mangled names line up. Both are only
used for OS-version feature checks (long-path handling, reserved device names, local-time
conversion).

**Upstream reads the version with `GetVersionEx`, which reports 6.2 on anything newer than Windows 8
unless the executable carries a compatibility manifest**, and then papers over that with a WMI query.
`RtlGetVersion` reports the real version with no manifest and no COM, so this uses it directly and
skips WMI entirely; ntdll is already on the link line via the Rust standard library. On failure it
claims Windows 10, the oldest version this build targets — every unrar caller treats a higher value
as "supported". Values are packed major/minor matching the `WNT_*` constants in `isnt.hpp` (e.g.
`0x0a00`). See `scripts/prepare-xwin.sh` for the rest of the unrar cross-compile fixes.

### `retro_log_shim.c`

No comments; it is the C side of the libretro log callback.

## Systems with no notes

`newsys/sinclair.rs`, `newsys/gameboy.rs`, `newsys/megadrive.rs`, `newsys/atari_2600.rs`,
`newsys/tic80.rs` and `newsys/amstrad.rs` are plain `System` implementations — an extension list, a
core name and a `default_meta` table — with nothing in them that needed explaining. `amstrad.rs`
carries commented-out `cap32_model`/`cap32_ram` options next to the live ones.

## Tests (`src/tests/`, `src/newsys/tests/`)

Convention: unit tests live beside the code as an out-of-line `mod tests` —
`#[cfg(test)] #[path = "tests/<module>_tests.rs"] mod tests;` at the bottom of the file. They are
still inner modules, so `use super::*` and private access work.

`tests/retro_emu_tests.rs` boots **real libretro cores** against demo content in the repo — one of
the reasons the ignored tests are ignored (network, GPU adapter, or a locally built PCem core plus
BIOS ROMs).

Facts the tests encode that are not stated elsewhere:

- **Two Amiga skeleton drives.** A plain executable is booted from a generated startup-sequence on a
  stock A500 (not through WHDLoad): `puae_use_whdload=disabled`, `puae_model=A500`. That 1.3 A500's
  skeleton is `system/ami13`; the AGA drive gets the larger `system/amihdd` with `lowlevel.library`
  (keyboard and joypad) in it, which no 1.3 machine ever had. The generated drive carries a `LIBS:`
  of its own because Kickstart has only *some* system libraries in ROM, and a demo that opens one of
  the others exits with no message at all when `OpenLibrary()` comes back empty.
- **A WHDLoad install is a `.slave` next to the data**; it turns WHDLoad on and needs an A1200.
- cbmconvert writes output relative to the CWD, so its test runs inside a temp dir. Its `-t` reads
  T64 input and `-N` writes native (a raw `.prg` with load address); the T64 entry named `BADALM`
  comes out lowercased, and a native `.prg` starts with a little-endian load address (`$0801`).
- `MIRROR_ROTATION` is **process-wide state**, so the rotation test puts it back afterwards; it uses
  `AmigascneFile`, the one class with three mirrors, and checks the list keeps its cyclic order — it
  is a rotation, not a move-to-front.
- The https→ftp space test exercises the real path: files.scene.org sends the space raw in
  `Location`, `Url::join` encodes it to `%20`, and the FTP side has to decode it again before `RETR`
  or the server 550s.
- The `e_lfanew` test covers a 64K intro packing both headers into one — `e_lfanew` points at `0x0c`
  so the PE header's own fields make up the rest of the DOS header — and confirms that an offset past
  the end of the file is a **DOS program with a field it never set**, not a Windows one whose image
  we failed to find.
- The resolution scan must reject a pack count, a version, a hex address and a texture size; with
  both an `x` and an `_` present, the `x` is the one that means a size. What an entry says was
  decided by a person and beats a file name. A DOS program is not the Windows system's, so nothing
  fills in a resolution for it — it runs under DOSBox, which has no such setting.
- `trunc_url`: components come off the **left**, and only when nothing is left to drop are both ends
  kept and the middle cut.
- ST pictures are reached by extension **and** by content; the screenshot test names files so the
  walk reaches the screenshot first and strips the extension so only the sniff can find the picture.

