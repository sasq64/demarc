use std::fmt::Write as FmtWrite;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use tracing::{info, warn};

const fn expand5(c: u8) -> u8 {
    (c << 3) | (c >> 2)
}

const fn expand6(c: u8) -> u8 {
    (c << 2) | (c >> 4)
}

/// Precomputed RGB565 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`. Replaces the per-pixel bit unpacking in
/// [`RetroCoreDirect::video_refresh`].
pub static RGB565_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 11) & 0x1f) as u8;
        let g6 = ((v >> 5) & 0x3f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand6(g6), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Precomputed 0RGB1555 → packed RGBA8888 table (256 KiB in rodata). Indexed by
/// the raw 16-bit pixel value; each entry is a `u32` whose native bytes are
/// `[r, g, b, 255]`.
pub static RGB1555_LUT: [u32; 65536] = {
    let mut lut = [0u32; 65536];
    let mut p = 0usize;
    while p < 65536 {
        let v = p as u16;
        let r5 = ((v >> 10) & 0x1f) as u8;
        let g5 = ((v >> 5) & 0x1f) as u8;
        let b5 = (v & 0x1f) as u8;
        lut[p] = u32::from_ne_bytes([expand5(r5), expand5(g5), expand5(b5), 255]);
        p += 1;
    }
    lut
};

/// Convert a 16-bits-per-pixel libretro framebuffer to packed RGBA8888 using
/// `lut`, which maps each raw 16-bit little-endian pixel to one output pixel.
/// `dst` must already be sized to `width * height`.
pub fn convert_16bpp(
    src: &[u8],
    dst: &mut [u32],
    width: usize,
    height: usize,
    pitch: usize,
    lut: &[u32; 65536],
) {
    for y in 0..height {
        let src_row = &src[y * pitch..y * pitch + width * 2];
        let dst_row = &mut dst[y * width..(y + 1) * width];
        for (out, px) in dst_row.iter_mut().zip(src_row.chunks_exact(2)) {
            let p = u16::from_le_bytes([px[0], px[1]]) as usize;
            *out = lut[p];
        }
    }
}

/// Per-frame picture statistics, meant for telling a demo that is running apart
/// from one that is idle or broken. Written out by [`FrameStatsLog`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameStats {
    // Average color in frame
    pub avg_color: u32,
    // Average difference from average color;
    // If every other pixel is black & white, avg would be gray,
    // and each pixel would differ 50% so this value would be 0.5 (which is max)
    // single colored screens gives 0.0
    pub color_diff: f32,

    // Change since last frame. The aveage difference between each pixel.
    // If every pixel changed from black to white, this will be 1.0
    pub frame_diff: f32,

    // Moving average of "did anything move", 0.0 to 1.0. A frame counts as
    // active as soon as a handful of pixels changed, so a moving starfield and
    // a full black-to-white flash both drive it towards 1.0; it only falls
    // while frames are identical.
    pub aggregated_diff: f32,
}

/// Weight of a single frame in `aggregated_diff`, i.e. a ~1 second moving
/// average at 60 fps.
const AGG_ALPHA: f32 = 1.0 / 60.0;

/// `aggregated_diff` down to which a picture still counts as moving, for
/// [`Backend::screen_changed`](crate::backend::Backend::screen_changed).
/// Identical frames take about four seconds to decay past it.
pub const SCREEN_ACTIVE: f32 = 0.02;

/// Per-channel step a pixel has to move before it counts as changed.
const PIXEL_CHANGE: u8 = 8;

/// Fraction of changed pixels that already counts as a fully active frame:
/// this much gives 63% activity, three times it 95%. Deliberately tiny, so
/// that how *much* of the picture moves barely matters.
const ACTIVE_FRACTION: f32 = 0.001;

/// Pixels per sampling chunk. Every kernel steps whole chunks and drops the
/// leftover pixels at the end of the frame, so for a given `skip` they all
/// look at exactly the same pixels.
const CHUNK: usize = 8;

/// Pixels to look at per frame. Past this the kernels step over whole chunks
/// instead of reading the framebuffer: both results are means over the picture,
/// and a regular sample estimates them just as well for a fraction of the work.
const SAMPLE_TARGET: usize = 1 << 18;

/// Chunks to step for a frame of `count` pixels. Kept odd so the stride cannot
/// line up with a power-of-two row width and sample the same few columns.
fn sample_skip(count: usize) -> usize {
    (count / SAMPLE_TARGET).max(1) | 1
}

/// Sum of every channel's absolute change, the number of pixels where some
/// channel moved more than [`PIXEL_CHANGE`], and how many pixels were looked
/// at. Alpha is deliberately ignored.
#[inline(always)]
fn diff_pixels_scalar(frame: &[u32], last_frame: &[u32], skip: usize) -> (u64, u64, usize) {
    let mut changed = 0u64;
    let mut moved = 0u64;
    let mut sampled = 0usize;
    let (chunks_a, _) = frame.as_chunks::<CHUNK>();
    let (chunks_b, _) = last_frame.as_chunks::<CHUNK>();
    for (ca, cb) in chunks_a.iter().zip(chunks_b).step_by(skip) {
        for (px, last) in ca.iter().zip(cb) {
            let [r, g, b, _] = px.to_ne_bytes();
            let [lr, lg, lb, _] = last.to_ne_bytes();
            let (dr, dg, db) = (r.abs_diff(lr), g.abs_diff(lg), b.abs_diff(lb));
            changed += dr as u64 + dg as u64 + db as u64;
            moved += (dr.max(dg).max(db) > PIXEL_CHANGE) as u64;
        }
        sampled += CHUNK;
    }
    (changed, moved, sampled)
}

/// See [`diff_pixels`]. `psadbw` sums the per-byte absolute differences of a
/// whole vector in one instruction, and the "did this pixel move" test falls
/// out of a saturating subtract plus a movemask: three bits per pixel, OR-ed
/// together and popcounted. Two vectors make up one [`CHUNK`].
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
fn diff_pixels_sse2(frame: &[u32], last_frame: &[u32], skip: usize) -> (u64, u64, usize) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    unsafe {
        let zero = _mm_setzero_si128();
        let rgb = _mm_set1_epi32(0x00ff_ffffu32 as i32);
        let step = _mm_set1_epi8(PIXEL_CHANGE as i8);
        let mut sad = zero;
        let mut moved = 0u64;
        let mut sampled = 0usize;

        let (chunks_a, _) = frame.as_chunks::<CHUNK>();
        let (chunks_b, _) = last_frame.as_chunks::<CHUNK>();
        for (ca, cb) in chunks_a.iter().zip(chunks_b).step_by(skip) {
            let (pa, pb) = (ca.as_ptr().cast::<__m128i>(), cb.as_ptr().cast::<__m128i>());
            for half in 0..2 {
                let a = _mm_and_si128(_mm_loadu_si128(pa.add(half)), rgb);
                let b = _mm_and_si128(_mm_loadu_si128(pb.add(half)), rgb);
                // |a - b| per byte, out of two saturating subtracts.
                let d = _mm_or_si128(_mm_subs_epu8(a, b), _mm_subs_epu8(b, a));
                sad = _mm_add_epi64(sad, _mm_sad_epu8(d, zero));
                // Saturating subtract leaves a non-zero byte exactly where the
                // channel moved more than PIXEL_CHANGE.
                let over = _mm_cmpeq_epi8(_mm_subs_epu8(d, step), zero);
                let bits = !(_mm_movemask_epi8(over) as u32);
                moved += ((bits | (bits >> 1) | (bits >> 2)) & 0x1111).count_ones() as u64;
            }
            sampled += CHUNK;
        }

        let changed =
            _mm_cvtsi128_si64(sad) as u64 + _mm_cvtsi128_si64(_mm_unpackhi_epi64(sad, sad)) as u64;
        (changed, moved, sampled)
    }
}

/// [`diff_pixels_sse2`] with a whole [`CHUNK`] per vector.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
fn diff_pixels_avx2(frame: &[u32], last_frame: &[u32], skip: usize) -> (u64, u64, usize) {
    #[cfg(target_arch = "x86")]
    use std::arch::x86::*;
    #[cfg(target_arch = "x86_64")]
    use std::arch::x86_64::*;

    unsafe {
        let zero = _mm256_setzero_si256();
        let rgb = _mm256_set1_epi32(0x00ff_ffffu32 as i32);
        let step = _mm256_set1_epi8(PIXEL_CHANGE as i8);
        let mut sad = zero;
        let mut moved = 0u64;
        let mut sampled = 0usize;

        let (chunks_a, _) = frame.as_chunks::<CHUNK>();
        let (chunks_b, _) = last_frame.as_chunks::<CHUNK>();
        for (ca, cb) in chunks_a.iter().zip(chunks_b).step_by(skip) {
            let a = _mm256_and_si256(_mm256_loadu_si256(ca.as_ptr().cast()), rgb);
            let b = _mm256_and_si256(_mm256_loadu_si256(cb.as_ptr().cast()), rgb);
            let d = _mm256_or_si256(_mm256_subs_epu8(a, b), _mm256_subs_epu8(b, a));
            sad = _mm256_add_epi64(sad, _mm256_sad_epu8(d, zero));
            let over = _mm256_cmpeq_epi8(_mm256_subs_epu8(d, step), zero);
            let bits = !(_mm256_movemask_epi8(over) as u32);
            moved += ((bits | (bits >> 1) | (bits >> 2)) & 0x1111_1111).count_ones() as u64;
            sampled += CHUNK;
        }

        let s = _mm_add_epi64(
            _mm256_castsi256_si128(sad),
            _mm256_extracti128_si256(sad, 1),
        );
        let changed =
            _mm_cvtsi128_si64(s) as u64 + _mm_cvtsi128_si64(_mm_unpackhi_epi64(s, s)) as u64;
        (changed, moved, sampled)
    }
}

/// `changed`/`moved`/`sampled` totals for two frames of equal length, looking
/// at one [`CHUNK`] in every `skip`. Dispatched at runtime like
/// [`convert_xrgb8888`], and for the same reason: we ship a baseline x86-64
/// binary. Measured over a whole 320x240 frame: 161 us scalar, 29 us with
/// baseline SSE2, 15 us with AVX2 (which doubles the pixels per `psadbw` and
/// per `movemask`, and is worth the second copy).
fn diff_pixels(frame: &[u32], last_frame: &[u32], skip: usize) -> (u64, u64, usize) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { diff_pixels_avx2(frame, last_frame, skip) };
        }
        if is_x86_feature_detected!("sse2") {
            return unsafe { diff_pixels_sse2(frame, last_frame, skip) };
        }
    }
    diff_pixels_scalar(frame, last_frame, skip)
}

/// Motion only: `(frame_diff, aggregated_diff)` as [`get_frame_stats`] would
/// compute them, without the two extra passes the average colour costs, and
/// from a [`SAMPLE_TARGET`]-pixel sample rather than the whole frame.
/// `last_frame` of a different length than `frame` counts as no change.
pub fn get_frame_diff(frame: &[u32], last_frame: &[u32], prev_aggregated: f32) -> (f32, f32) {
    let decayed = prev_aggregated - prev_aggregated * AGG_ALPHA;
    if frame.is_empty() || last_frame.len() != frame.len() {
        return (0.0, decayed);
    }

    let (changed, moved, sampled) = diff_pixels(frame, last_frame, sample_skip(frame.len()));
    // Fewer pixels than one chunk: nothing was looked at.
    if sampled == 0 {
        return (0.0, decayed);
    }

    // Both are means over the sample, scaled so that a full black-to-white
    // swing is 1.0.
    let fraction = moved as f32 / sampled as f32;
    let activity = 1.0 - (-fraction / ACTIVE_FRACTION).exp();
    (
        changed as f32 / (sampled as f32 * 3.0 * 255.0),
        prev_aggregated + (activity - prev_aggregated) * AGG_ALPHA,
    )
}

/// Statistics for one frame of packed RGBA8888 pixels. `prev_aggregated` is the
/// `aggregated_diff` of the previous frame (`0.0` to start); `last_frame` of a
/// different length than `frame` counts as no change. Only [`FrameStatsLog`]
/// wants the average colour; everything else takes [`get_frame_diff`].
pub fn get_frame_stats(frame: &[u32], last_frame: &[u32], prev_aggregated: f32) -> FrameStats {
    let (frame_diff, aggregated_diff) = get_frame_diff(frame, last_frame, prev_aggregated);
    let count = frame.len();
    if count == 0 {
        return FrameStats {
            aggregated_diff,
            ..FrameStats::default()
        };
    }

    let (mut sum_r, mut sum_g, mut sum_b) = (0u64, 0u64, 0u64);
    for px in frame {
        let [r, g, b, _] = px.to_ne_bytes();
        sum_r += r as u64;
        sum_g += g as u64;
        sum_b += b as u64;
    }
    let n = count as u64;
    let (avg_r, avg_g, avg_b) = ((sum_r / n) as u8, (sum_g / n) as u8, (sum_b / n) as u8);

    let mut spread = 0u64;
    for px in frame {
        let [r, g, b, _] = px.to_ne_bytes();
        spread += r.abs_diff(avg_r) as u64 + g.abs_diff(avg_g) as u64 + b.abs_diff(avg_b) as u64;
    }

    FrameStats {
        avg_color: u32::from_ne_bytes([avg_r, avg_g, avg_b, 255]),
        // A mean over every channel of every pixel, scaled so that a full
        // black-to-white swing is 1.0.
        color_diff: spread as f32 / (count as f32 * 3.0 * 255.0),
        frame_diff,
        aggregated_diff,
    }
}

/// Number of [`FrameStatsLog`]s opened so far, so concurrent views do not write
/// over each other.
static STATS_LOGS: AtomicUsize = AtomicUsize::new(0);

/// Appends one CSV line per frame to the file named by `DEMARC_FRAME_STATS`,
/// for `scripts/plot_frame_stats.py` to graph. Every line is one unbuffered
/// `write`, so whatever was logged is on its way to disk even if the process is
/// killed rather than dropping anything.
pub struct FrameStatsLog {
    file: File,
    start: Instant,
    line: String,
}

impl FrameStatsLog {
    /// `None` unless `DEMARC_FRAME_STATS` names a file to write. A second
    /// concurrent log gets `<path>.1`, a third `<path>.2`, and so on.
    pub fn from_env() -> Option<Self> {
        let path = PathBuf::from(std::env::var_os("DEMARC_FRAME_STATS")?);
        let no = STATS_LOGS.fetch_add(1, Ordering::Relaxed);
        let path = if no == 0 {
            path
        } else {
            PathBuf::from(format!("{}.{no}", path.display()))
        };
        match File::create(&path) {
            Ok(mut file) => {
                let _ = file.write_all(
                    b"frame,time_ms,width,height,avg_r,avg_g,avg_b,color_diff,frame_diff,aggregated_diff\n",
                );
                info!("Logging frame stats to {}", path.display());
                Some(Self {
                    file,
                    start: Instant::now(),
                    line: String::new(),
                })
            }
            Err(e) => {
                warn!("Could not open frame stat log {}: {e}", path.display());
                None
            }
        }
    }

    pub fn log(&mut self, frame_no: u64, width: usize, height: usize, stats: &FrameStats) {
        let [r, g, b, _] = stats.avg_color.to_ne_bytes();
        let ms = self.start.elapsed().as_secs_f64() * 1000.0;
        // Formatted into a buffer first: writing straight to the unbuffered
        // file would be one `write` syscall per piece of the format string.
        self.line.clear();
        let _ = writeln!(
            self.line,
            "{frame_no},{ms:.3},{width},{height},{r},{g},{b},{:.6},{:.6},{:.6}",
            stats.color_diff, stats.frame_diff, stats.aggregated_diff
        );
        let _ = self.file.write_all(self.line.as_bytes());
    }
}

/// Convert an XRGB8888 libretro framebuffer to packed RGBA8888.
/// `dst` must already be sized to `width * height`.
///
/// The source is BGRA in memory (little-endian XRGB8888), so every pixel is a
/// pure byte permutation of its destination: as a `u32` it is `0xAARRGGBB` and
/// we want `0xAABBGGRR`, which is `swap_bytes()` followed by `rotate_right(8)`.
///
/// Spelling it that way rather than as `from_ne_bytes([px[2], px[1], px[0],
/// px[3]])` is the whole point: LLVM recognises bswap+rotate as a byte shuffle
/// and vectorizes it, while the byte-at-a-time version stays scalar at four
/// `movzbl`s, three shifts and three `or`s per pixel. Measured on a 320x240
/// frame: 37 us scalar, 11 us with baseline SSE2 (two shuffles plus a
/// shift/or per 4 pixels), 3.3 us with SSSE3 (one `pshufb` per 4 pixels),
/// and 2.4 us with AVX2 (one `vpshufb` per 8).
#[inline(always)]
fn convert_xrgb8888_impl(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    for y in 0..height {
        let src_row = &src[y * pitch..y * pitch + width * 4];
        let dst_row = &mut dst[y * width..(y + 1) * width];
        // Indexed rather than `iter_mut().zip(chunks_exact(4))` on purpose: the
        // `Zip::new` call does not get inlined without LTO, and an unvectorized
        // loop body is the entire cost here.
        for x in 0..width {
            let v = u32::from_ne_bytes([
                src_row[x * 4],
                src_row[x * 4 + 1],
                src_row[x * 4 + 2],
                src_row[x * 4 + 3],
            ]);
            dst_row[x] = v.swap_bytes().rotate_right(8);
        }
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
fn convert_xrgb8888_avx2(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "ssse3")]
fn convert_xrgb8888_ssse3(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

/// See [`convert_xrgb8888_impl`]. We ship a baseline x86-64 binary, so the
/// `pshufb` that makes this a single instruction per 4 pixels is only reachable
/// through runtime dispatch. `is_x86_feature_detected!` caches its answer in an
/// atomic, so the per-frame cost is a relaxed load.
pub fn convert_xrgb8888(src: &[u8], dst: &mut [u32], width: usize, height: usize, pitch: usize) {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return unsafe { convert_xrgb8888_avx2(src, dst, width, height, pitch) };
        }
        if is_x86_feature_detected!("ssse3") {
            return unsafe { convert_xrgb8888_ssse3(src, dst, width, height, pitch) };
        }
    }
    convert_xrgb8888_impl(src, dst, width, height, pitch)
}

#[cfg(test)]
#[path = "tests/pixels_tests.rs"]
mod tests;
