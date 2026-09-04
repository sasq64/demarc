//! The Luau script that draws the music backend's picture.
//!
//! A music file has no video of its own, so [`MusicEmu`](crate::music_emu::MusicEmu)
//! draws something: historically a hard-coded oscilloscope, now whatever
//! `system/lua/scope.lua` says. The backend keeps doing the audio work — most
//! importantly maintaining the delay line that makes the trace line up with what
//! the speakers are playing — and this module hands the result to a script that
//! owns every pixel.
//!
//! Luau rather than PUC-Lua for its native `buffer` type: the script draws into a
//! byte-addressed block with `buffer.writeu32` rather than crossing the FFI once
//! per pixel. See [`Visualizer::render`] for the round trip and the
//! byte-order argument that makes it work.
//!
//! Everything the script can ask about lives in [`VisData`], behind a mutex,
//! because the Rust closures registered as Lua globals outlive any one call and
//! (with mlua's `send` feature) must be `Send`. The backend fills that struct in
//! before each frame; the script reads it back through `get_samples()` and
//! friends.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result, anyhow};
use mlua::{AnyUserData, Buffer, Function, Lua, LuaOptions, StdLib, Table, UserData, Variadic};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};
use tracing::{info, warn};

/// Window the FFT behind `get_spectrum()` runs over. At 44.1kHz this is ~23ms,
/// a shade under two 60Hz frames — long enough to resolve the bass notes a
/// spectrum display is mostly about, short enough to still look responsive.
const FFT_SIZE: usize = 1024;

/// What the script sees. Refreshed by the backend before every
/// [`Visualizer::render`]; the buffers are reused, so filling it in costs no
/// allocation after the first frame.
#[derive(Default)]
pub struct VisData {
    /// The window to draw, interleaved stereo in `-1.0..1.0`. This is the
    /// *delayed* audio — what is coming out of the speakers now, not what was
    /// just rendered — so a trace drawn from it lines up with what is heard.
    pub samples: Vec<f32>,
    /// Player metadata as key/value pairs (`title`, `composer`, `format`, …),
    /// snapshotted by the backend because `musix` only offers it through a
    /// `&mut self` call that a `'static` Lua closure could never hold.
    ///
    /// Values are ISO-8859-1 bytes, not UTF-8: that is the character set
    /// [`Font::row`] indexes by, so a title reaches the script in the encoding
    /// it will be drawn in. See `music_emu::to_latin1`.
    pub meta: Vec<(String, Vec<u8>)>,
    /// Frames rendered since the song loaded. Doubles as the cache key for
    /// [`Fft`], so two `get_spectrum()` calls in one frame do one transform.
    pub frame_count: u64,
    /// Seconds of song played.
    pub time: f64,
    pub sample_rate: f64,
}

/// Scratch for `get_spectrum()`, kept between calls so a frame's transform
/// allocates nothing.
struct Fft {
    plan: Arc<dyn RealToComplex<f32>>,
    input: Vec<f32>,
    output: Vec<Complex32>,
    scratch: Vec<Complex32>,
    /// Hann window, precomputed.
    hann: Vec<f32>,
    /// The last result, and the `(frame_count, bins)` it was computed for.
    cached: Vec<f32>,
    cached_for: Option<(u64, usize)>,
}

impl Fft {
    fn new() -> Self {
        let plan = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let hann = (0..FFT_SIZE)
            .map(|i| {
                let t = i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
            })
            .collect();
        Self {
            input: plan.make_input_vec(),
            output: plan.make_output_vec(),
            scratch: plan.make_scratch_vec(),
            plan,
            hann,
            cached: Vec::new(),
            cached_for: None,
        }
    }

    /// Magnitudes for the current window, grouped into `bins` log-spaced
    /// buckets. Roughly `0..1` for a full-scale tone.
    ///
    /// Log spacing because a linear split of the 513 raw bins puts every note a
    /// chip tune actually plays into the first handful of them, leaving most of
    /// a spectrum display permanently flat.
    fn spectrum(&mut self, data: &VisData, bins: usize) -> &[f32] {
        if self.cached_for == Some((data.frame_count, bins)) {
            return &self.cached;
        }

        // Mono downmix of the last FFT_SIZE stereo pairs. Early in a song there
        // are fewer than that, so the window opens on leading silence rather
        // than on whatever the uninitialised tail held.
        let pairs = data.samples.len() / 2;
        for i in 0..FFT_SIZE {
            let sample = if i + pairs >= FFT_SIZE {
                let idx = (i + pairs - FFT_SIZE) * 2;
                (data.samples[idx] + data.samples[idx + 1]) * 0.5
            } else {
                0.0
            };
            self.input[i] = sample * self.hann[i];
        }

        // `process_with_scratch` uses `input` as scratch too, so its contents
        // are rubbish afterwards -- fine, it is rewritten every call.
        if let Err(e) =
            self.plan
                .process_with_scratch(&mut self.input, &mut self.output, &mut self.scratch)
        {
            warn!("FFT failed: {e}");
            self.cached.clear();
            self.cached.resize(bins, 0.0);
            self.cached_for = Some((data.frame_count, bins));
            return &self.cached;
        }

        // 2/N turns a bin magnitude into the amplitude of the sinusoid that
        // produced it; the other factor of 2 undoes the Hann window's coherent
        // gain of 0.5, so a full-scale tone lands near 1.0.
        let scale = 4.0 / FFT_SIZE as f32;
        // Bin 0 is DC, which is not a frequency anyone wants to see drawn.
        let lo = 1.0f32;
        let hi = (self.output.len() - 1) as f32;
        self.cached.clear();
        for b in 0..bins {
            let edge = |k: usize| lo * (hi / lo).powf(k as f32 / bins as f32);
            let start = edge(b) as usize;
            let end = (edge(b + 1).ceil() as usize)
                .max(start + 1)
                .min(self.output.len());
            let peak = self.output[start..end]
                .iter()
                .map(|c| c.norm())
                .fold(0.0f32, f32::max);
            self.cached.push(peak * scale);
        }
        self.cached_for = Some((data.frame_count, bins));
        &self.cached
    }
}

/// A loaded script, its interpreter, and the frame it draws into.
///
/// Recreated wholesale on reload, so nothing the previous script left behind in
/// a global can leak into the next one.
struct Script {
    /// Held only to keep the state alive: `buffer` and `render` reference it.
    _lua: Lua,
    /// The frame the script draws into: `width * height * 4` bytes, allocated
    /// once and handed to every `render` call.
    buffer: Buffer,
    render: Function,
}

pub struct Visualizer {
    width: usize,
    height: usize,
    path: PathBuf,
    script: Script,
    shared: Arc<Mutex<VisData>>,
    /// Set by the file watcher, cleared by [`Self::render`].
    reload: Arc<AtomicBool>,
    /// Held only to keep the watch alive; dropping it stops the notifications.
    _watcher: RecommendedWatcher,
    /// Readback scratch for [`Self::render`], reused so the per-frame copy out
    /// of the Lua buffer does not allocate.
    bytes: Vec<u8>,
}

impl Visualizer {
    /// Load `path` and prepare it to draw `width` x `height` frames.
    ///
    /// Fails if the script is missing, does not compile, or defines no
    /// `render` function. The caller decides what that means: for the music
    /// backend it is not fatal — the song plays on with a black picture.
    pub fn new(path: &Path, width: usize, height: usize) -> Result<Self> {
        let shared = Arc::new(Mutex::new(VisData::default()));
        let reload = Arc::new(AtomicBool::new(false));
        let watcher = watch(path, reload.clone())?;
        let script = load(path, width, height, &shared)?;
        info!("Loaded visualization script {path:?}");
        Ok(Self {
            width,
            height,
            path: path.to_path_buf(),
            script,
            shared,
            reload,
            _watcher: watcher,
            bytes: Vec::with_capacity(width * height * 4),
        })
    }

    /// The data the next `render` will show the script. The backend writes
    /// through this before each frame.
    pub fn data(&self) -> MutexGuard<'_, VisData> {
        // A poisoned lock would mean a Lua closure panicked mid-call. Nothing
        // here is left half-written by that, so carry on with the data rather
        // than taking the whole song down.
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Run the script and copy its frame into `out`, which must be
    /// `width * height` pixels.
    ///
    /// Reloads the script first if it changed on disk. A reload that fails to
    /// compile is logged and the previous script kept: half-saved files are a
    /// normal thing to observe while someone is editing, and the last working
    /// visualization is better than a black screen.
    pub fn render(&mut self, out: &mut [u32]) -> Result<()> {
        if self.reload.swap(false, Ordering::Relaxed) {
            match load(&self.path, self.width, self.height, &self.shared) {
                Ok(script) => {
                    info!("Reloaded visualization script {:?}", self.path);
                    self.script = script;
                }
                Err(e) => warn!("Keeping previous script; reload failed: {e:#}"),
            }
        }

        self.script
            .render
            .call::<()>(&self.script.buffer)
            .context("Render() failed")?;

        // Read the frame back out. `Buffer` exposes no borrow, so this is a
        // copy either way; going through the cursor rather than `to_vec` at
        // least reuses our own allocation.
        self.bytes.clear();
        self.script
            .buffer
            .clone()
            .cursor()
            .read_to_end(&mut self.bytes)
            .context("reading the frame back from Lua")?;

        // The script wrote colours built by `rgb()`, which packs them for
        // Luau's little-endian `buffer.writeu32` -- so the bytes in the buffer
        // are already `[r, g, b, a]`. Reading each group of four back as a
        // native-order `u32` is exactly the packing the frontend wants (see
        // `backend::frame_bytes`), on either endianness.
        for (dst, src) in out.iter_mut().zip(self.bytes.chunks_exact(4)) {
            *dst = u32::from_ne_bytes([src[0], src[1], src[2], src[3]]);
        }
        Ok(())
    }
}

/// Watch `path` for changes, setting `flag` when it moves.
///
/// Watches the *directory*, not the file: editors overwhelmingly save by
/// writing a temporary file and renaming it over the target, which replaces the
/// inode and leaves a watch on the file itself pointing at the old one — the
/// first save would be seen and no other. Filtering the directory's events by
/// file name survives that.
///
/// No debouncing: a burst of events for one save collapses into a single
/// boolean, and the reload it triggers happens once, on the next frame.
fn watch(path: &Path, flag: Arc<AtomicBool>) -> Result<RecommendedWatcher> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{path:?} has no file name"))?
        .to_os_string();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            return;
        }
        if event.paths.iter().any(|p| p.file_name() == Some(&name)) {
            flag.store(true, Ordering::Relaxed);
        }
    })
    .with_context(|| format!("watching {dir:?}"))?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watching {dir:?}"))?;
    Ok(watcher)
}

/// Build a fresh interpreter, register the API, and run `path`.
fn load(path: &Path, width: usize, height: usize, shared: &Arc<Mutex<VisData>>) -> Result<Script> {
    let source = std::fs::read_to_string(path).with_context(|| format!("reading {path:?}"))?;

    // Luau has no `io` or `package` to begin with; naming the set we do want
    // keeps it that way if that ever changes. `os` is in for `os.clock`.
    let lua = Lua::new_with(
        StdLib::STRING
            | StdLib::TABLE
            | StdLib::MATH
            | StdLib::BIT
            | StdLib::BUFFER
            | StdLib::VECTOR
            | StdLib::UTF8
            | StdLib::OS,
        LuaOptions::default(),
    )
    .context("creating the Lua state")?;

    let buffer = lua
        .create_buffer_with_capacity(width * height * 4)
        .context("allocating the frame buffer")?;

    register(&lua, width, height, shared)?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "scope.lua".into());
    lua.load(source)
        .set_name(name)
        .exec()
        .with_context(|| format!("running {path:?}"))?;

    let globals = lua.globals();
    // Optional: somewhere for a script to precompute tables before frame one.
    if let Ok(init) = globals.get::<Function>("Init") {
        init.call::<()>(()).context("Init() failed")?;
    }
    // Looked up once rather than per frame.
    let render = globals
        .get::<Function>("Render")
        .with_context(|| format!("{path:?} defines no Render(buffer) function"))?;

    Ok(Script {
        _lua: lua,
        buffer,
        render,
    })
}

/// Register the globals the script draws with. Every closure here is `Send`
/// (mlua's `send` feature requires it) and owns a handle to the shared data
/// rather than borrowing the backend, which it could not outlive.
fn register(lua: &Lua, width: usize, height: usize, shared: &Arc<Mutex<VisData>>) -> Result<()> {
    let globals = lua.globals();
    globals.set("WIDTH", width)?;
    globals.set("HEIGHT", height)?;

    // Colour packing. The frontend wants each pixel's *memory* bytes to be
    // `[r, g, b, a]`; Luau's `buffer.writeu32` is little-endian. Packing it this
    // way makes `writeu32` lay those four bytes down in that order, which is
    // what the readback in `Visualizer::render` expects. A script that builds
    // `0xAARRGGBB` by hand instead will have its channels swapped.
    globals.set(
        "rgb",
        lua.create_function(|_, (r, g, b, a): (i64, i64, i64, Option<i64>)| {
            // Masked rather than range-checked, and signed on the way in: a
            // script deriving a colour from a waveform will hand over a
            // negative or an overshoot sooner or later, and erroring there
            // would blank the frame over one bad pixel.
            let byte = |v: i64| (v & 0xff) as u32;
            Ok(byte(r) | byte(g) << 8 | byte(b) << 16 | byte(a.unwrap_or(255)) << 24)
        })?,
    )?;

    let data = shared.clone();
    globals.set(
        "get_samples",
        lua.create_function(move |lua, ()| {
            let data = data.lock().unwrap_or_else(|e| e.into_inner());
            lua.create_sequence_from(data.samples.iter().copied())
        })?,
    )?;

    let data = shared.clone();
    let fft = Arc::new(Mutex::new(Fft::new()));
    globals.set(
        "get_spectrum",
        lua.create_function(move |lua, bins: usize| {
            let bins = bins.clamp(1, 4096);
            let data = data.lock().unwrap_or_else(|e| e.into_inner());
            let mut fft = fft.lock().unwrap_or_else(|e| e.into_inner());
            lua.create_sequence_from(fft.spectrum(&data, bins).iter().copied())
        })?,
    )?;

    let data = shared.clone();
    globals.set(
        "get_meta",
        lua.create_function(move |lua, ()| {
            let data = data.lock().unwrap_or_else(|e| e.into_inner());
            let table = lua.create_table_with_capacity(0, data.meta.len())?;
            for (key, value) in &data.meta {
                table.set(key.as_str(), lua.create_string(value)?)?;
            }
            table.set("sample_rate", data.sample_rate)?;
            Ok::<Table, mlua::Error>(table)
        })?,
    )?;

    let data = shared.clone();
    globals.set(
        "get_frame_count",
        lua.create_function(move |_, ()| {
            Ok(data.lock().unwrap_or_else(|e| e.into_inner()).frame_count)
        })?,
    )?;

    let data = shared.clone();
    globals.set(
        "get_time",
        lua.create_function(move |_, ()| Ok(data.lock().unwrap_or_else(|e| e.into_inner()).time))?,
    )?;

    register_noise(lua)?;

    register_drawing(lua, width, height)
}

/// `noise([...])`: a pseudo-random number in `0.0..1.0`.
///
/// Called with no arguments it draws from a stream, so every call is a fresh
/// value -- sparks, dust, jitter. Called with numbers it is a *hash* of them:
/// the same arguments always give the same result, which is what a script wants
/// for anything that must stay put from one frame to the next (the brightness
/// of a star at `noise(x, y)`, the phase of a bar at `noise(bin)`) and which a
/// stream cannot give without storing a number per thing.
///
/// Luau's `math.random` is registered too and is the better tool for a plain
/// die roll; this exists for the hashed form, and for the fact that the stream
/// survives nothing -- a reload starts a new Lua state either way.
fn register_noise(lua: &Lua) -> Result<()> {
    /// SplitMix64's finalizer: enough avalanche that neighbouring pixel
    /// coordinates give unrelated values, which is the whole point of hashing
    /// them rather than scaling them.
    fn mix(mut x: u64) -> u64 {
        x ^= x >> 30;
        x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x ^= x >> 27;
        x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^ (x >> 31)
    }

    // The stream's state. Arbitrary but fixed, so a script that looks right
    // once looks right again.
    let state = Arc::new(Mutex::new(0x853c_49e6_748f_ea9bu64));
    lua.globals().set(
        "noise",
        lua.create_function(move |_, args: Variadic<f64>| {
            let bits = if args.is_empty() {
                let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                mix(*state)
            } else {
                // Folded rather than summed so that `noise(1, 2)` and
                // `noise(2, 1)` differ, and `+ 0.0` so that a coordinate that
                // came out as -0.0 hashes like the 0.0 it equals.
                args.iter().fold(0xcbf2_9ce4_8422_2325, |acc, &v| {
                    mix(acc ^ (v + 0.0).to_bits())
                })
            };
            // The top 53 bits are the ones a double can hold exactly, so this
            // is every representable value in [0, 1) with equal probability.
            Ok((bits >> 11) as f64 / (1u64 << 53) as f64)
        })?,
    )?;
    Ok(())
}

/// A bitmap font in the headerless "raw" format Amiga font packs ship in: 256
/// glyphs stored back to back, one byte per pixel row, most significant bit
/// leftmost, so glyph `c` starts at byte `c * height`.
///
/// Nothing in the file says how tall a glyph is -- there is no header at all --
/// so the height comes from the length. The Topaz files are 4096 bytes, i.e.
/// 8x16: the 8x8 Amiga font with every row doubled, which is what gives it the
/// right proportions on a square-pixel display.
#[derive(Clone, Copy)]
struct Font {
    glyphs: &'static [u8],
    height: usize,
}

/// Glyphs are 8 pixels wide in every raw font of this shape: one byte, one row.
const FONT_WIDTH: usize = 8;

/// What `load_font` will hand out, by name.
///
/// Embedded rather than read from disk: the Lua state deliberately has no `io`,
/// and letting a script name a path would be the one hole in that. It also
/// means a visualization draws text whether or not the system directory
/// survived being moved. See src/fonts/README for provenance.
const FONTS: &[(&str, &[u8])] = &[
    // Kickstart 2.x/3.x topaz.font, thinner and squarer.
    ("topaz", include_bytes!("fonts/topaz1200.raw")),
    ("potnoodle", include_bytes!("fonts/pot_noodle.raw")),
    ("microknight", include_bytes!("fonts/microknight.raw")),
];

impl Font {
    fn new(glyphs: &'static [u8]) -> Result<Self> {
        let height = glyphs.len() / 256;
        if height == 0 || !glyphs.len().is_multiple_of(256) {
            return Err(anyhow!(
                "{} bytes is not 256 glyphs of a whole number of rows",
                glyphs.len()
            ));
        }
        Ok(Self { glyphs, height })
    }

    /// Row `row` of the glyph for byte `ch`, bit 7 the leftmost pixel.
    ///
    /// The character set is whatever the font file uses, which for these is
    /// ISO-8859-1 -- so a Lua string is indexed byte by byte, and a script that
    /// hands over UTF-8 gets mojibake rather than an error.
    fn row(&self, ch: u8, row: usize) -> u8 {
        self.glyphs[ch as usize * self.height + row]
    }
}

/// The handle a script holds. Copying it is copying a slice reference, so a
/// script may keep one in a global and hand it to every `text` call.
#[derive(Clone, Copy)]
struct LuaFont(Font);

impl UserData for LuaFont {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        // Enough for a script to lay out lines and centre a string without
        // hard-coding the size of a font it asked for by name.
        fields.add_field_method_get("width", |_, _| Ok(FONT_WIDTH));
        fields.add_field_method_get("height", |_, f| Ok(f.0.height));
    }
}

/// Bulk drawing primitives.
///
/// A script *can* do all of this with `buffer.writeu32`, and for scattered
/// pixels it should. These exist for the filled areas: a faithful oscilloscope
/// draws a vertical run per column and a spectrum draws a bar per bin, which
/// together are up to `WIDTH * HEIGHT` writes a frame, each one an FFI crossing
/// plus a bounds check. Filling a scratch row in Rust and handing it over one
/// `write_bytes` per rectangle row turns the whole frame into a few hundred
/// crossings instead of a few hundred thousand.
fn register_drawing(lua: &Lua, width: usize, height: usize) -> Result<()> {
    let globals = lua.globals();
    // Shared by both, so a frame's drawing reuses one allocation.
    let scratch = Arc::new(Mutex::new(Vec::<u8>::new()));

    /// The `n` copies of `colour` that a span of that length is written from.
    fn run(scratch: &mut Vec<u8>, colour: u32, n: usize) -> &[u8] {
        let bytes = colour.to_le_bytes();
        scratch.clear();
        scratch.reserve(n * 4);
        for _ in 0..n {
            scratch.extend_from_slice(&bytes);
        }
        scratch
    }

    let pad = scratch.clone();
    globals.set(
        "clear",
        lua.create_function(move |_, (buf, colour): (Buffer, u32)| {
            let mut scratch = pad.lock().unwrap_or_else(|e| e.into_inner());
            buf.write_bytes(0, run(&mut scratch, colour, width * height));
            Ok(())
        })?,
    )?;

    let pad = scratch.clone();
    globals.set(
        "box",
        lua.create_function(
            move |_, (buf, x, y, w, h, colour): (Buffer, i64, i64, i64, i64, u32)| {
                // Clipped rather than rejected: a script computing coordinates
                // from a waveform will run off the edge now and then, and an
                // error there would blank the whole frame.
                let Some((x, y, w, h)) = clip(x, y, w, h, width, height) else {
                    return Ok(());
                };
                let mut scratch = pad.lock().unwrap_or_else(|e| e.into_inner());
                let row = run(&mut scratch, colour, w);
                for row_y in y..y + h {
                    buf.write_bytes((row_y * width + x) * 4, row);
                }
                Ok(())
            },
        )?,
    )?;

    globals.set(
        "load_font",
        lua.create_function(|_, name: String| {
            let (_, glyphs) = FONTS
                .iter()
                .find(|(known, _)| *known == name)
                .ok_or_else(|| {
                    let known: Vec<_> = FONTS.iter().map(|(n, _)| *n).collect();
                    mlua::Error::runtime(format!(
                        "no font named {name:?}; there is {}",
                        known.join(" and ")
                    ))
                })?;
            Font::new(glyphs)
                .map(LuaFont)
                .map_err(|e| mlua::Error::runtime(format!("{name}: {e}")))
        })?,
    )?;

    let pad = scratch.clone();
    globals.set(
        "text",
        lua.create_function(
            move |_,
                  (buf, font, x, y, string, colour): (
                Buffer,
                AnyUserData,
                i64,
                i64,
                mlua::String,
                u32,
            )| {
                let font = font.borrow::<LuaFont>()?.0;
                let mut scratch = pad.lock().unwrap_or_else(|e| e.into_inner());
                for (i, ch) in string.as_bytes().iter().enumerate() {
                    let gx = x + (i * FONT_WIDTH) as i64;
                    // Clipped per glyph, then per run: a script drawing a song
                    // title has no idea how wide it is until it is too late.
                    if gx >= width as i64 || gx + FONT_WIDTH as i64 <= 0 {
                        continue;
                    }
                    for row in 0..font.height {
                        let py = y + row as i64;
                        if py < 0 || py >= height as i64 {
                            continue;
                        }
                        // Only the set pixels are written, so text lands over
                        // whatever is already there. They come in horizontal
                        // runs -- doubled pixels make those at least two wide
                        // -- and one `write_bytes` per run beats one FFI
                        // crossing per pixel.
                        let bits = font.row(*ch, row);
                        let mut col = 0;
                        while col < FONT_WIDTH {
                            if bits & (0x80 >> col) == 0 {
                                col += 1;
                                continue;
                            }
                            let start = col;
                            while col < FONT_WIDTH && bits & (0x80 >> col) != 0 {
                                col += 1;
                            }
                            let x0 = (gx + start as i64).max(0);
                            let x1 = (gx + col as i64).min(width as i64);
                            if x1 > x0 {
                                let pixels = run(&mut scratch, colour, (x1 - x0) as usize);
                                buf.write_bytes((py as usize * width + x0 as usize) * 4, pixels);
                            }
                        }
                    }
                }
                Ok(())
            },
        )?,
    )?;
    Ok(())
}

/// A rectangle clipped to a `width` x `height` frame, or `None` if none of it
/// lands on screen.
///
/// A negative extent is empty rather than mirrored: `w` is a width, and a
/// script that computed a negative one has a bug that silently drawing
/// something would only hide.
fn clip(
    x: i64,
    y: i64,
    w: i64,
    h: i64,
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let (x0, y0) = (x.max(0), y.max(0));
    let x1 = x.saturating_add(w).min(width as i64);
    let y1 = y.saturating_add(h).min(height as i64);
    (x1 > x0 && y1 > y0).then(|| {
        (
            x0 as usize,
            y0 as usize,
            (x1 - x0) as usize,
            (y1 - y0) as usize,
        )
    })
}

#[cfg(test)]
#[path = "tests/music_vis_tests.rs"]
mod tests;
