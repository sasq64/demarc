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
mod tests {
    use super::*;

    /// Write `source` to a uniquely named file so the tests can run in
    /// parallel, and hand back a visualizer for it.
    fn vis(name: &str, source: &str) -> Result<(Visualizer, PathBuf)> {
        vis_sized(name, source, 4, 2)
    }

    fn vis_sized(
        name: &str,
        source: &str,
        width: usize,
        height: usize,
    ) -> Result<(Visualizer, PathBuf)> {
        let path = std::env::temp_dir().join(format!("music_vis_{name}.lua"));
        std::fs::write(&path, source).unwrap();
        Visualizer::new(&path, width, height).map(|v| (v, path))
    }

    /// The whole round trip in one assertion: a colour built by `rgb()`, laid
    /// down by Luau's little-endian `buffer.writeu32`, read back as a
    /// native-order `u32`, must equal what the backend's own `rgb` produces.
    /// Get the byte order wrong and red and blue swap.
    #[test]
    fn a_pixel_survives_the_round_trip() {
        let (x, y) = (2usize, 1usize);
        let (mut v, path) = vis(
            "pixel",
            r#"
            function Render(buf)
                buffer.writeu32(buf, (1 * WIDTH + 2) * 4, rgb(0x11, 0x22, 0x33))
            end
            "#,
        )
        .unwrap();

        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();

        let expected = u32::from_ne_bytes([0x11, 0x22, 0x33, 0xff]);
        assert_eq!(frame[y * 4 + x], expected, "wrong colour or wrong position");
        assert!(
            frame.iter().filter(|&&px| px == expected).count() == 1,
            "the pixel landed more than once"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `rgb`'s alpha defaults to opaque, and the drawing helpers agree with
    /// `writeu32` about the packing.
    #[test]
    fn the_helpers_and_writeu32_agree() {
        let (mut v, path) = vis(
            "helpers",
            r#"
            function Render(buf)
                clear(buf, rgb(1, 2, 3))
                box(buf, 0, 0, WIDTH, 1, rgb(4, 5, 6, 7))
            end
            "#,
        )
        .unwrap();

        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();

        let cleared = u32::from_ne_bytes([1, 2, 3, 255]);
        let line = u32::from_ne_bytes([4, 5, 6, 7]);
        assert!(frame[..4].iter().all(|&px| px == line), "box: {frame:?}");
        assert!(
            frame[4..].iter().all(|&px| px == cleared),
            "clear: {frame:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Rectangles that run off the edge are clipped, not rejected: a script
    /// deriving coordinates from a waveform will overshoot now and then, and
    /// blanking the whole frame over it would be a poor trade.
    #[test]
    fn out_of_range_boxes_are_clipped() {
        let (mut v, path) = vis(
            "clip",
            r#"
            function Render(buf)
                clear(buf, rgb(0, 0, 0))
                box(buf, -100, 0, 200, 1, rgb(9, 9, 9))     -- straddles the left edge
                box(buf, 2, -5, 1, 500, rgb(8, 8, 8))       -- taller than the frame
                box(buf, 0, 999, 2, 2, rgb(7, 7, 7))        -- entirely off-screen
                box(buf, 0, 0, -4, -4, rgb(6, 6, 6))        -- negative extents
            end
            "#,
        )
        .unwrap();

        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).expect("clipping must not error");
        assert_eq!(
            frame[3],
            u32::from_ne_bytes([9, 9, 9, 255]),
            "clipped to the right edge"
        );
        assert_eq!(
            frame[4 + 2],
            u32::from_ne_bytes([8, 8, 8, 255]),
            "clipped to the bottom"
        );
        for absent in [[7, 7, 7, 255], [6, 6, 6, 255]] {
            assert!(
                !frame.iter().any(|&px| px == u32::from_ne_bytes(absent)),
                "an empty rectangle drew something: {absent:?}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A script that does not compile fails at load, rather than panicking or
    /// producing a `Visualizer` that throws on every frame.
    #[test]
    fn a_syntax_error_fails_to_load() {
        let Err(err) = vis("syntax", "function Render(buf) this is not lua") else {
            panic!("a script that does not compile must not load");
        };
        assert!(
            format!("{err:#}").contains("music_vis_syntax.lua"),
            "the error should name the script: {err:#}"
        );
    }

    /// So does one that never defines `render` — better caught at load than as
    /// a blank window.
    #[test]
    fn a_missing_render_fails_to_load() {
        let Err(err) = vis("norender", "function nope() end") else {
            panic!("a script without render() must not load");
        };
        assert!(
            format!("{err:#}").contains("Render"),
            "the error should mention render: {err:#}"
        );
    }

    /// A script that throws mid-frame is a per-call error, not a load error:
    /// the caller decides what to show, and can keep asking.
    #[test]
    fn a_throwing_render_errors_per_call() {
        let (mut v, path) = vis("throw", "function Render(buf) error('boom') end").unwrap();
        let mut frame = vec![0u32; 4 * 2];
        assert!(v.render(&mut frame).is_err());
        assert!(
            v.render(&mut frame).is_err(),
            "the second call must also fail"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `init` runs once before the first frame, and what it leaves in a global
    /// is still there when `render` looks.
    #[test]
    fn init_runs_before_the_first_frame() {
        let (mut v, path) = vis(
            "Init",
            r#"
            function Init() COLOUR = rgb(3, 4, 5) end
            function Render(buf) clear(buf, COLOUR) end
            "#,
        )
        .unwrap();
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        assert!(
            frame
                .iter()
                .all(|&px| px == u32::from_ne_bytes([3, 4, 5, 255]))
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The script sees the samples the backend put there, at the scale the
    /// documentation promises.
    #[test]
    fn get_samples_reaches_the_script() {
        let (mut v, path) = vis(
            "samples",
            r#"
            function Render(buf)
                local s = get_samples()
                clear(buf, rgb(#s, math.floor(s[1] * 100), math.floor(s[2] * 100)))
            end
            "#,
        )
        .unwrap();

        v.data().samples = vec![0.5, -0.25];
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        // -0.25 * 100 floors to -25, which wraps to 231 as a byte.
        assert_eq!(frame[0], u32::from_ne_bytes([2, 50, 231, 255]));
        let _ = std::fs::remove_file(&path);
    }

    /// Metadata the backend snapshotted reaches the script as a table, keyed by
    /// name, with the sample rate alongside it.
    #[test]
    fn get_meta_reaches_the_script() {
        let (mut v, path) = vis(
            "meta",
            r#"
            function Render(buf)
                local m = get_meta()
                assert(m.composer == "Rob Hubbard", "composer was " .. tostring(m.composer))
                assert(m.sample_rate == 44100, "rate was " .. tostring(m.sample_rate))
                assert(m.nope == nil, "invented a key")
                clear(buf, rgb(#m.title, string.byte(m.title, 1), 0))
            end
            "#,
        )
        .unwrap();

        {
            let mut data = v.data();
            data.sample_rate = 44100.0;
            data.meta = vec![
                ("title".into(), b"Commando".to_vec()),
                ("composer".into(), b"Rob Hubbard".to_vec()),
            ];
        }
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        // "Commando" is 8 characters and starts with 'C' (67).
        assert_eq!(frame[0], u32::from_ne_bytes([8, 67, 0, 255]));
        let _ = std::fs::remove_file(&path);
    }

    /// A full-scale tone shows up in the spectrum, and only near its own
    /// frequency. Also pins the per-frame caching: two calls, one transform.
    #[test]
    fn get_spectrum_finds_a_tone() {
        let (mut v, path) = vis(
            "spectrum",
            r#"
            function Render(buf)
                local a = get_spectrum(16)
                local b = get_spectrum(16)
                local peak, at = 0, 0
                for i = 1, #a do
                    assert(a[i] == b[i], "cached spectrum differs")
                    if a[i] > peak then peak, at = a[i], i end
                end
                clear(buf, rgb(at, math.floor(peak * 100), #a))
            end
            "#,
        )
        .unwrap();

        // A tone at one eighth of the sample rate, as interleaved stereo.
        {
            let mut data = v.data();
            data.sample_rate = 44100.0;
            data.samples = (0..FFT_SIZE)
                .flat_map(|i| {
                    let s = (std::f32::consts::TAU * i as f32 / 8.0).sin();
                    [s, s]
                })
                .collect();
        }
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();

        let [bin, peak, bins, _] = frame[0].to_ne_bytes();
        assert_eq!(bins, 16, "wrong number of bins");
        // Hann-corrected, a full-scale sine should come back near 1.0.
        assert!((80..=120).contains(&(peak as i32)), "peak was {peak}/100");
        // Sample rate / 8 is the top eighth of the spectrum, so the last few
        // log-spaced buckets.
        assert!(bin >= 13, "the tone landed in bucket {bin} of 16");
        let _ = std::fs::remove_file(&path);
    }

    /// Editing the script on disk replaces the running one, without the caller
    /// doing anything but rendering the next frame.
    #[test]
    fn saving_the_script_reloads_it() {
        let (mut v, path) = vis(
            "reload",
            "function Render(buf) clear(buf, rgb(1, 1, 1)) end",
        )
        .unwrap();
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        assert_eq!(frame[0], u32::from_ne_bytes([1, 1, 1, 255]));

        // Saved the way editors actually save: write a temporary file and
        // rename it over the target. That replaces the inode, which is exactly
        // what a watch on the file itself would fail to follow -- so this is
        // the case the directory watch exists for.
        let tmp = path.with_extension("lua.tmp");
        std::fs::write(&tmp, "function Render(buf) clear(buf, rgb(2, 2, 2)) end").unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        // The watcher is a background thread; give it a moment to notice rather
        // than assuming the write and the notification are ordered.
        let reloaded = (0..100).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            v.render(&mut frame).unwrap();
            frame[0] == u32::from_ne_bytes([2, 2, 2, 255])
        });
        assert!(reloaded, "the script was never reloaded");
        let _ = std::fs::remove_file(&path);
    }

    /// A save that leaves the file broken keeps the last working script, rather
    /// than blanking the window over what is probably a half-typed edit.
    #[test]
    fn a_broken_reload_keeps_the_old_script() {
        let (mut v, path) = vis(
            "badreload",
            "function Render(buf) clear(buf, rgb(1, 1, 1)) end",
        )
        .unwrap();
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();

        std::fs::write(&path, "function Render(buf) this is not lua").unwrap();
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            v.render(&mut frame)
                .expect("a broken reload must not fail the frame");
            assert_eq!(
                frame[0],
                u32::from_ne_bytes([1, 1, 1, 255]),
                "the broken script took over"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// A glyph lands where it was asked to, the right way round: the leftmost
    /// pixel is the *most* significant bit of the row byte, and getting that
    /// backwards mirrors every letter. Topaz's `A` starts `..##....`-doubled,
    /// so row 0 is 0x18 -- pixels 3 and 4 and nothing else.
    #[test]
    fn text_draws_a_glyph() {
        let (mut v, path) = vis_sized(
            "text",
            r#"
            function Init() FONT = load_font("topaz") end
            function Render(buf)
                clear(buf, rgb(0, 0, 0))
                text(buf, FONT, 0, 0, "A", rgb(255, 255, 255))
                assert(FONT.width == 8 and FONT.height == 16, "wrong metrics")
            end
            "#,
            16,
            16,
        )
        .unwrap();

        let mut frame = vec![0u32; 16 * 16];
        v.render(&mut frame).unwrap();

        let ink = u32::from_ne_bytes([255, 255, 255, 255]);
        // Every row of the glyph cell, read back as the byte the font holds.
        let drawn: Vec<u8> = (0..16)
            .map(|y| {
                (0..8).fold(0u8, |bits, x| {
                    bits | (((frame[y * 16 + x] == ink) as u8) << (7 - x))
                })
            })
            .collect();
        let font = Font::new(FONTS[0].1).unwrap();
        let expected: Vec<u8> = (0..16).map(|row| font.row(b'A', row)).collect();
        assert_eq!(drawn, expected, "the glyph came out wrong");
        // Nothing outside the 8-pixel cell, so `text` is not painting a box.
        assert!(
            (0..16).all(|y| (8..16).all(|x| frame[y * 16 + x] != ink)),
            "text drew past the glyph cell"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Characters advance by the glyph width, and text that runs off any edge is
    /// clipped rather than erroring -- a song title is as long as it is, and the
    /// script cannot know before it asks.
    #[test]
    fn text_advances_and_clips() {
        let (mut v, path) = vis_sized(
            "textclip",
            r#"
            function Init() FONT = load_font("topaz") end
            function Render(buf)
                clear(buf, rgb(0, 0, 0))
                text(buf, FONT, 0, 0, " A", rgb(255, 255, 255))  -- second cell
                text(buf, FONT, -8, 0, "A!", rgb(1, 2, 3))       -- first cell off left
                text(buf, FONT, 28, 0, "AA", rgb(4, 5, 6))       -- runs off the right
                text(buf, FONT, 0, -20, "A", rgb(7, 8, 9))       -- above the frame
                text(buf, FONT, 0, 100, "A", rgb(9, 8, 7))       -- below it
            end
            "#,
            32,
            16,
        )
        .expect("text off the edge must not fail to load");

        let mut frame = vec![0u32; 32 * 16];
        v.render(&mut frame).expect("clipping must not error");

        let font = Font::new(FONTS[0].1).unwrap();
        let ink = u32::from_ne_bytes([255, 255, 255, 255]);
        // ' ' is blank, so the 'A' sits in the second cell: x 8..16.
        for row in 0..16 {
            for col in 0..8 {
                let set = font.row(b'A', row) & (0x80 >> col) != 0;
                assert_eq!(
                    frame[row * 32 + 8 + col] == ink,
                    set,
                    "advance is wrong at row {row}, column {col}"
                );
            }
        }
        // '!' is the second character of the string starting at -8, so it lands
        // in the first cell; the 'A' before it is entirely off-screen.
        let bang = u32::from_ne_bytes([1, 2, 3, 255]);
        assert!(
            (0..16).any(|row| (0..8).any(|col| frame[row * 32 + col] == bang)),
            "the character straddling the left edge vanished"
        );
        // Half a glyph over the right edge draws its left half and stops.
        let right = u32::from_ne_bytes([4, 5, 6, 255]);
        assert!(
            (0..16).any(|row| (28..32).any(|col| frame[row * 32 + col] == right)),
            "the character straddling the right edge vanished"
        );
        for absent in [[7, 8, 9, 255], [9, 8, 7, 255]] {
            assert!(
                !frame.iter().any(|&px| px == u32::from_ne_bytes(absent)),
                "text off the top or bottom drew something: {absent:?}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// Asking for a font that is not there is an error the script can see, and
    /// it names the ones that are -- rather than handing back something that
    /// draws blanks forever.
    #[test]
    fn an_unknown_font_errors() {
        let (mut v, path) = vis(
            "badfont",
            r#"
            function Render(buf)
                local ok, err = pcall(load_font, "helvetica")
                -- tostring: what pcall catches here is the host's error object,
                -- not a plain string.
                err = tostring(err)
                assert(not ok, "an unknown font loaded")
                assert(string.find(err, "topaz") ~= nil, "the error should list the fonts: " .. err)
                clear(buf, rgb(1, 2, 3))
            end
            "#,
        )
        .unwrap();
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        assert_eq!(frame[0], u32::from_ne_bytes([1, 2, 3, 255]));
        let _ = std::fs::remove_file(&path);
    }

    /// `noise()` stays in range, hands back something different every call,
    /// and -- given arguments -- the *same* thing every time, which is what a
    /// script placing stars relies on.
    #[test]
    fn noise_is_in_range_and_hashes_its_arguments() {
        let (mut v, path) = vis(
            "noise",
            r#"
            function Render(buf)
                local lo, hi, distinct = 1, 0, {}
                for i = 1, 1000 do
                    local n = noise()
                    assert(n >= 0 and n < 1, "out of range: " .. tostring(n))
                    lo, hi = math.min(lo, n), math.max(hi, n)
                    distinct[n] = true
                end
                local count = 0
                for _ in pairs(distinct) do count = count + 1 end
                assert(count > 990, "the stream repeats itself: " .. count)
                assert(lo < 0.05 and hi > 0.95, "not spread over 0..1")

                assert(noise(1, 2) == noise(1, 2), "hashed noise is not stable")
                assert(noise(1, 2) ~= noise(2, 1), "argument order is ignored")
                assert(noise(3) ~= noise(4), "neighbours hash the same")
                assert(noise(0) == noise(-0), "-0 and 0 hash differently")
                clear(buf, rgb(math.floor(noise(7) * 255), 0, 0))
            end
            "#,
        )
        .unwrap();
        let mut frame = vec![0u32; 4 * 2];
        v.render(&mut frame).unwrap();
        // Frame to frame the hashed value is the same, so the colour is too.
        let first = frame[0];
        v.render(&mut frame).unwrap();
        assert_eq!(frame[0], first, "noise(7) changed between frames");
        let _ = std::fs::remove_file(&path);
    }

    /// Every embedded font is the shape the drawing code assumes.
    #[test]
    fn the_embedded_fonts_decode() {
        for (name, glyphs) in FONTS {
            let font = Font::new(glyphs).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(font.height, 16, "{name} is not 8x16");
            assert_eq!(font.row(b' ', 0), 0, "{name}: space is not blank");
            assert!(
                (0..font.height).any(|row| font.row(b'A', row) != 0),
                "{name}: 'A' is blank"
            );
        }
    }
}
