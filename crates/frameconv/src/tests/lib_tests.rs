use super::*;

const W: u32 = 320;
const H: u32 = 256;
const BG: u32 = 0xff30_2010;
const RECT: u32 = 0xffe0_4020;
const RECT_SIZE: (i32, i32) = (40, 30);

/// The pixel the debug pipeline writes for the fallback path.
const FALLBACK: u32 = 0xff00_0003;
const PATHS: [&str; 4] = ["static", "motion", "occlusion", "fallback"];

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Pipelines,
}

fn gpu() -> Option<Gpu> {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let Ok(adapter) = pollster::block_on(instance.request_adapter(&Default::default())) else {
        eprintln!("no GPU adapter, skipping");
        return None;
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&Default::default())).ok()?;
    let pipelines = Pipelines::new(&device);
    Some(Gpu {
        device,
        queue,
        pipelines,
    })
}

impl Gpu {
    fn read(&self, texture: &wgpu::Texture) -> Vec<u32> {
        let (w, h) = (texture.width(), texture.height());
        let row = (w * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (row * h) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: None,
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        let mapped = buffer.get_mapped_range(..);
        let mut pixels = vec![0u32; (w * h) as usize];
        let bytes: &mut [u8] = bytemuck::cast_slice_mut(&mut pixels);
        for (out, line) in bytes
            .chunks_exact_mut((w * 4) as usize)
            .zip(mapped.chunks_exact(row as usize))
        {
            out.copy_from_slice(&line[..(w * 4) as usize]);
        }
        pixels
    }

    fn render(&self, conv: &mut Converter, a: &Pixels, b: &Pixels, n: u32) -> Vec<u32> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        conv.render(&self.pipelines, &self.queue, &mut encoder, a, b, n);
        self.queue.submit([encoder.finish()]);
        self.read(conv.six())
    }

    /// The path each pixel of the same conversion took.
    fn paths(&self, conv: &mut Converter, a: &Pixels, b: &Pixels, n: u32) -> Vec<u32> {
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let pipeline = &self.pipelines.interp_paths;
        conv.render_with(
            pipeline,
            &self.pipelines,
            &self.queue,
            &mut encoder,
            a,
            b,
            n,
        );
        self.queue.submit([encoder.finish()]);
        self.read(conv.six())
    }
}

/// A static background with a solid rectangle moving over it.
#[derive(Clone)]
struct Scene {
    bg: Vec<u32>,
    start: (i32, i32),
    /// Native pixels per source frame.
    velocity: (i32, i32),
}

fn uniform() -> Vec<u32> {
    vec![BG; (W * H) as usize]
}

fn noise() -> Vec<u32> {
    let mut state = 0x2545_f491u32;
    (0..W * H)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state | 0xff00_0000
        })
        .collect()
}

fn checker(cell: u32) -> Vec<u32> {
    (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            if (x / cell + y / cell).is_multiple_of(2) {
                BG
            } else {
                0xff80_8080
            }
        })
        .collect()
}

fn fill_rect(pixels: &mut [u32], width: i32, x: i32, y: i32, size: (i32, i32)) {
    for row in y..y + size.1 {
        let row = (row * width) as usize;
        pixels[row + x as usize..row + (x + size.0) as usize].fill(RECT);
    }
}

fn scale(frame: &[u32]) -> Vec<u32> {
    let mut six = Vec::with_capacity(frame.len() * (SCALE * SCALE) as usize);
    for line in frame.chunks_exact(W as usize) {
        let row: Vec<u32> = line.iter().flat_map(|p| [*p; SCALE as usize]).collect();
        for _ in 0..SCALE {
            six.extend_from_slice(&row);
        }
    }
    six
}

impl Scene {
    fn source(&self, frame: u64) -> Pixels {
        let f = frame as i32;
        let mut pixels = self.bg.clone();
        fill_rect(
            &mut pixels,
            W as i32,
            self.start.0 + self.velocity.0 * f,
            self.start.1 + self.velocity.1 * f,
            RECT_SIZE,
        );
        Arc::new(pixels)
    }

    /// The display frames for which the rectangle stays on screen, up to 120.
    fn frames(&self) -> u64 {
        let room = |start: i32, v: i32, size: i32, limit: u32| match v.signum() {
            0 => 1000,
            1 => (limit as i32 - size - start) / v,
            _ => start / -v,
        };
        let source = room(self.start.0, self.velocity.0, RECT_SIZE.0, W).min(room(
            self.start.1,
            self.velocity.1,
            RECT_SIZE.1,
            H,
        )) as u64;
        ((source - 1) * 6 / 5).min(120)
    }

    fn expected(&self, background: &[u32], k: u64) -> Vec<u32> {
        let s = SCALE as i32;
        let step = 5 * k as i32;
        let mut six = background.to_vec();
        fill_rect(
            &mut six,
            (W * SCALE) as i32,
            self.start.0 * s + self.velocity.0 * step,
            self.start.1 * s + self.velocity.1 * step,
            (RECT_SIZE.0 * s, RECT_SIZE.1 * s),
        );
        six
    }

    /// Every display frame, pixel for pixel. Returns the frames with a pixel
    /// that fell back.
    fn check(&self, gpu: &Gpu) -> usize {
        let mut conv = Converter::new(&gpu.device, &gpu.pipelines, W, H);
        let background = scale(&self.bg);
        let mut cadence = Cadence::default();
        let mut pair = (self.source(0), self.source(1));
        let mut pair_at = 0;
        let mut fallback = 0;
        let mut failures = Vec::new();
        for k in 0..self.frames() {
            let (a, n) = cadence.next();
            if a != pair_at {
                pair = (pair.1.clone(), self.source(a + 1));
                pair_at = a;
            }
            let got = gpu.render(&mut conv, &pair.0, &pair.1, n);
            let want = self.expected(&background, k);
            let paths = gpu.paths(&mut conv, &pair.0, &pair.1, n);
            fallback += paths.contains(&FALLBACK) as usize;
            if got == want {
                continue;
            }
            let wrong = got.iter().zip(&want).filter(|(g, w)| g != w).count();
            let first = got.iter().zip(&want).position(|(g, w)| g != w).unwrap();
            let (x, y) = (first as u32 % (W * SCALE), first as u32 / (W * SCALE));
            failures.push(format!(
                "frame {k} ({a}+{n}/6): {wrong} wrong, first at ({x}, {y}) expected {:08x} got {:08x} via {}",
                want[first], got[first], PATHS[paths[first] as u8 as usize],
            ));
        }
        assert!(
            failures.is_empty(),
            "{} frames wrong of {}:\n{}",
            failures.len(),
            self.frames(),
            failures[..failures.len().min(8)].join("\n"),
        );
        fallback
    }
}

const STARTS: [(i32, i32); 4] = [(20, 40), (260, 40), (100, 20), (100, 206)];

/// One scene per direction, each at `speed`.
fn directions(bg: &[u32], speed: i32) -> Vec<Scene> {
    [(1, 0), (-1, 0), (0, 1), (0, -1)]
        .into_iter()
        .zip(STARTS)
        .map(|((x, y), start)| Scene {
            bg: bg.to_vec(),
            start,
            velocity: (x * speed, y * speed),
        })
        .collect()
}

#[test]
fn shaders_validate() {
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    for source in [
        include_str!("../me.wgsl"),
        include_str!("../interp.wgsl"),
        include_str!("../resolve.wgsl"),
    ] {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
        Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .unwrap();
    }
}

#[test]
fn cadence_steps_five_sources_in_six_displays() {
    let mut cadence = Cadence::default();
    for k in 0..600 {
        assert_eq!(cadence.peek(), cadence.clone().next());
        let (a, n) = cadence.next();
        assert_eq!(a * 6 + n as u64, 5 * k);
    }
    let mut cadence = Cadence::default();
    let phases: Vec<u32> = (0..6).map(|_| cadence.next().1).collect();
    assert_eq!(phases, [0, 5, 4, 3, 2, 1]);
    assert_eq!(cadence.next(), (5, 0));
}

#[test]
fn a_rect_moves_exactly_in_every_direction() {
    let Some(gpu) = gpu() else { return };
    for scene in directions(&uniform(), 1) {
        assert_eq!(scene.check(&gpu), 0);
    }
}

#[test]
fn a_rect_crossing_tile_borders_stays_exact() {
    let Some(gpu) = gpu() else { return };
    // Both edges start just short of a tile border, on each side of the tile.
    for (x, speed) in [(14, 1), (15, 1), (16, 1), (9, 4), (11, 4)] {
        let scene = Scene {
            bg: uniform(),
            start: (x, 40),
            velocity: (speed, 0),
        };
        assert_eq!(scene.check(&gpu), 0);
    }
}

#[test]
fn phase_zero_is_the_scaled_source() {
    let Some(gpu) = gpu() else { return };
    let scene = directions(&noise(), 1).remove(0);
    let mut conv = Converter::new(&gpu.device, &gpu.pipelines, W, H);
    for a in [0, 5, 10] {
        let (frame, next) = (scene.source(a), scene.source(a + 1));
        assert!(gpu.render(&mut conv, &frame, &next, 0) == scale(&frame));
    }
}

#[test]
fn a_textured_background_stays_exact() {
    let Some(gpu) = gpu() else { return };
    for bg in [noise(), checker(1), checker(2), checker(8)] {
        for scene in directions(&bg, 1) {
            assert_eq!(scene.check(&gpu), 0);
        }
    }
}

#[test]
fn faster_rects_stay_exact() {
    let Some(gpu) = gpu() else { return };
    for bg in [uniform(), noise()] {
        for speed in [2, 4] {
            for scene in directions(&bg, speed) {
                assert_eq!(scene.check(&gpu), 0);
            }
        }
    }
}

#[test]
fn a_resolve_is_the_box_mean_of_the_target() {
    let Some(gpu) = gpu() else { return };
    let scene = directions(&noise(), 1).remove(0);
    let mut conv = Converter::new(&gpu.device, &gpu.pipelines, W, H);
    let six = gpu.render(&mut conv, &scene.source(3), &scene.source(4), 2);
    let native = gpu.read(conv.native());
    let s = SCALE as usize;
    for (i, pixel) in native.iter().enumerate() {
        let (x, y) = (i % W as usize, i / W as usize);
        for channel in 0..3 {
            let channel_of = |p: u32| (p >> (channel * 8)) as u8 as u32;
            let sum: u32 = (0..s * s)
                .map(|j| six[(y * s + j / s) * W as usize * s + x * s + j % s])
                .map(channel_of)
                .sum();
            let mean = sum as f32 / (s * s) as f32;
            assert!(
                (channel_of(*pixel) as f32 - mean).abs() <= 1.0,
                "({x}, {y})"
            );
        }
    }
}

const BLACK: u32 = 0xff00_0000;

/// 5x7 letters, one row per byte, with a mix of thin and crowded strokes.
const GLYPHS: [[u8; 7]; 8] = [
    [0x0e, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11], // A
    [0x1f, 0x10, 0x10, 0x1e, 0x10, 0x10, 0x1f], // E
    [0x11, 0x11, 0x11, 0x1f, 0x11, 0x11, 0x11], // H
    [0x1f, 0x04, 0x04, 0x04, 0x04, 0x04, 0x1f], // I
    [0x11, 0x1b, 0x15, 0x15, 0x11, 0x11, 0x11], // M
    [0x0e, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0e], // O
    [0x0f, 0x10, 0x10, 0x0e, 0x01, 0x01, 0x1e], // S
    [0x11, 0x11, 0x11, 0x15, 0x15, 0x1b, 0x11], // W
];

/// `lines` rows of `columns` letters on black, in a colour gradient per row.
struct Text {
    width: usize,
    height: usize,
    pixels: Vec<u32>,
    six: Vec<u32>,
}

impl Text {
    fn new(columns: usize, lines: usize, scale: usize, seed: u32) -> Self {
        let cell = (6 * scale, 9 * scale);
        let (width, height) = (columns * cell.0, lines * cell.1);
        let mut pixels = vec![BLACK; width * height];
        let mut state = seed;
        for line in 0..lines {
            for column in 0..columns {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let glyph = &GLYPHS[(state >> 24) as usize % GLYPHS.len()];
                for y in 0..7 * scale {
                    for x in 0..5 * scale {
                        if glyph[y / scale] >> (4 - x / scale) & 1 == 1 {
                            let row = line * cell.1 + y;
                            let shade = 0x60 + (y * 0x90 / (7 * scale)) as u32;
                            pixels[row * width + column * cell.0 + x] =
                                BLACK | shade << 16 | 0xc0 << 8 | (0xff - shade);
                        }
                    }
                }
            }
        }
        let s = SCALE as usize;
        let mut six = Vec::with_capacity(pixels.len() * s * s);
        for row in pixels.chunks_exact(width) {
            let wide: Vec<u32> = row.iter().flat_map(|p| [*p; SCALE as usize]).collect();
            for _ in 0..s {
                six.extend_from_slice(&wide);
            }
        }
        Self {
            width,
            height,
            pixels,
            six,
        }
    }

    /// Copy `rows` rows starting at (`x`, `y`) of the text to row `to` of `frame`.
    fn draw(
        &self,
        frame: &mut [u32],
        to: usize,
        (x, y): (usize, usize),
        rows: usize,
        scale: usize,
    ) {
        let (from, width, height) = if scale == 1 {
            (&self.pixels, self.width, W as usize)
        } else {
            (&self.six, self.width * scale, (W * SCALE) as usize)
        };
        for row in 0..rows {
            let line = &from[(y + row) * width + x..];
            frame[(to + row) * height..][..height].copy_from_slice(&line[..height]);
        }
    }
}

/// Native pixels at the frame's sides left out of a measure: text entering there
/// is in only one of the two frames.
const SIDE: usize = 8;

/// The mean wrong pixels per moved frame, within the 6x rows `rows`.
fn wrong_per_frame(
    gpu: &Gpu,
    source: &dyn Fn(u64) -> Vec<u32>,
    expected: &dyn Fn(u64) -> Vec<u32>,
    rows: std::ops::Range<usize>,
    frames: u64,
) -> usize {
    let width = (W * SCALE) as usize;
    let mut conv = Converter::new(&gpu.device, &gpu.pipelines, W, H);
    let mut cadence = Cadence::default();
    let mut pair = (Arc::new(source(0)), Arc::new(source(1)));
    let mut pair_at = 0;
    let (mut wrong, mut moved) = (0, 0);
    let mut by_path = [0; PATHS.len()];
    for k in 0..frames {
        let (a, n) = cadence.next();
        if a != pair_at {
            pair = (pair.1.clone(), Arc::new(source(a + 1)));
            pair_at = a;
        }
        if n == 0 {
            continue;
        }
        let got = gpu.render(&mut conv, &pair.0, &pair.1, n);
        let paths = gpu.paths(&mut conv, &pair.0, &pair.1, n);
        let want = expected(k);
        moved += 1;
        for row in rows.clone() {
            let (got, want) = (&got[row * width..][..width], &want[row * width..][..width]);
            if got == want {
                continue;
            }
            for x in SIDE * SCALE as usize..width - SIDE * SCALE as usize {
                if got[x] != want[x] {
                    wrong += 1;
                    by_path[paths[row * width + x] as u8 as usize] += 1;
                }
            }
        }
    }
    println!(
        "{} wrong/frame, by path {by_path:?} of {PATHS:?}",
        wrong / moved
    );
    wrong / moved
}

fn frame_size(scale: usize) -> usize {
    (W as usize * scale) * (H as usize * scale)
}

fn scroller(gpu: &Gpu, speed: usize, scale: usize) -> usize {
    let text = Text::new(160, 1, scale, 7);
    let rows = text.height;
    let source = |f: u64| {
        let mut frame = vec![BLACK; frame_size(1)];
        text.draw(&mut frame, 40, (f as usize * speed, 0), rows, 1);
        frame
    };
    let expected = |k: u64| {
        let mut six = vec![BLACK; frame_size(SCALE as usize)];
        let at = (5 * k as usize * speed, 0);
        text.draw(&mut six, 240, at, rows * SCALE as usize, SCALE as usize);
        six
    };
    wrong_per_frame(gpu, &source, &expected, 0..(H * SCALE) as usize, 30)
}

fn opposite_lines(gpu: &Gpu, speed: usize) -> usize {
    let (top, bottom) = (Text::new(160, 1, 2, 7), Text::new(160, 1, 2, 99));
    let rows = top.height;
    // The lines share a 16 px tile but no 8 px one.
    let (first, second) = (40, 40 + rows + 2);
    let from = |f: usize| 400 - f * speed;
    let source = |f: u64| {
        let mut frame = vec![BLACK; frame_size(1)];
        top.draw(&mut frame, first, (f as usize * speed, 0), rows, 1);
        bottom.draw(&mut frame, second, (from(f as usize), 0), rows, 1);
        frame
    };
    let expected = |k: u64| {
        let s = SCALE as usize;
        let mut six = vec![BLACK; frame_size(s)];
        let step = 5 * k as usize * speed;
        top.draw(&mut six, first * s, (step, 0), rows * s, s);
        bottom.draw(&mut six, second * s, (from(0) * s - step, 0), rows * s, s);
        six
    };
    wrong_per_frame(gpu, &source, &expected, 0..(H * SCALE) as usize, 30)
}

/// Rows entering at the frame's edges cannot be interpolated, so those are left out.
fn vertical_text(gpu: &Gpu, speed: usize) -> usize {
    let text = Text::new(60, 40, 2, 7);
    let rows = H as usize;
    let source = |f: u64| {
        let mut frame = vec![BLACK; frame_size(1)];
        text.draw(&mut frame, 0, (0, f as usize * speed), rows, 1);
        frame
    };
    let expected = |k: u64| {
        let s = SCALE as usize;
        let mut six = vec![BLACK; frame_size(s)];
        text.draw(&mut six, 0, (0, 5 * k as usize * speed), rows * s, s);
        six
    };
    let edge = 8 * SCALE as usize;
    let inner = edge..(H * SCALE) as usize - edge;
    wrong_per_frame(gpu, &source, &expected, inner, 30)
}

#[test]
fn a_text_scroller_stays_close_to_exact() {
    let Some(gpu) = gpu() else { return };
    for (scale, limits) in [
        (1, [0, 220, 60, 570]),
        (2, [0, 0, 10, 580]),
        (3, [0, 0, 0, 10]),
    ] {
        for (speed, limit) in (1..).zip(limits) {
            let wrong = scroller(&gpu, speed, scale);
            assert!(
                wrong <= limit,
                "scale {scale} speed {speed}: {wrong} > {limit}"
            );
        }
    }
}

#[test]
fn opposite_lines_stay_close_to_exact() {
    let Some(gpu) = gpu() else { return };
    for (speed, limit) in (1..).zip([5400, 4100, 1900, 3100]) {
        let wrong = opposite_lines(&gpu, speed);
        assert!(wrong <= limit, "speed {speed}: {wrong} > {limit}");
    }
}

#[test]
fn vertical_text_is_exact_away_from_the_edges() {
    let Some(gpu) = gpu() else { return };
    for speed in 1..=4 {
        assert_eq!(vertical_text(&gpu, speed), 0);
    }
}
