use std::fs;
use std::path::Path;

use super::System;

const CORE_NAME_PCEM: &str = "pcem";

/// PC/DOS through PCem.
///
/// Content is a PCem machine `.cfg` — the same file the desktop PCem writes
/// into its `configs/` directory and takes with `--config`. It names the
/// machine, CPU, video and sound cards and the disc images to mount, so it is
/// the whole of the configuration; the core has no machine picker.
///
/// BIOS ROMs are not shipped (they are copyrighted) and must be placed under
/// `<system dir>/pcem/roms/<machine>/`; `docs/roms.txt` in the PCem tree lists
/// what each machine needs. Everything the machine writes — NVR, logs — goes
/// under `<save dir>/pcem/`.
pub struct PcSystem {}

/// Does this look like a PCem machine config?
///
/// `.cfg` is far too generic an extension to accept on its own — plenty of
/// systems drop one next to their content — so require the one key every PCem
/// machine config has and nothing else uses: a `model =` naming the machine.
fn is_pcem_config(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let line = line.trim();
        line.strip_prefix("model")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| !value.trim().is_empty())
    })
}

impl System for PcSystem {
    fn extensions(&self) -> &'static [&'static str] {
        &["cfg"]
    }

    fn can_load(&self, path: &Path) -> bool {
        self.handles_ext(path) && is_pcem_config(path)
    }

    fn core_name(&self) -> &'static str {
        CORE_NAME_PCEM
    }

    fn name(&self) -> &'static str {
        "PC"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use clap::Parser;

    use crate::Args;
    use crate::frontend::system_dir;
    use crate::libretro::RETROK_RETURN;
    use crate::newsys::NewSys;

    fn write(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    /// `.cfg` is a name half the world uses, so the sniff has to lean on the
    /// `model =` key rather than the extension.
    #[test]
    fn tells_a_machine_config_from_any_other_cfg() {
        let dir = tempfile::tempdir().unwrap();
        let sys = PcSystem {};

        let pcem = write(
            dir.path(),
            "486.cfg",
            "model = ami486\ncpu = 0\nmem_size = 16384\ngfxcard = tgui9440\n",
        );
        assert!(sys.can_load(&pcem));

        // Section headers and spacing vary between hand-written configs.
        let spaced = write(dir.path(), "xt.cfg", "\n[Machine]\n  model=ibmxt\n");
        assert!(sys.can_load(&spaced));

        // Some other emulator's settings file that happens to end in .cfg.
        let other = write(dir.path(), "other.cfg", "fullscreen = 1\nscale = 2\n");
        assert!(!sys.can_load(&other));

        // A `model` key with nothing after it configures no machine.
        let empty = write(dir.path(), "empty.cfg", "model = \n");
        assert!(!sys.can_load(&empty));

        // A key that merely starts with "model" is not the model key.
        let lookalike = write(dir.path(), "look.cfg", "model_name = foo\n");
        assert!(!sys.can_load(&lookalike));
    }

    /// How long to give the machine. The XT counts all 640K before it looks
    /// for something to boot, which takes about 35 emulated seconds — it
    /// reaches BASIC around frame 2400.
    const BOOT_FRAME_LIMIT: usize = 4000;

    /// Decode one frame in this many. Reading 2000 character cells is not free
    /// in a debug build, and neither milestone this test looks for is on
    /// screen for less than a second.
    const DECODE_EVERY: usize = 4;

    /// Wall-clock ceiling, so a core that stops producing frames fails the test
    /// instead of hanging it.
    const TIMEOUT: Duration = Duration::from_secs(180);

    /// Boot a real IBM PC/XT, BIOS and all, and read the screen back.
    ///
    /// Ignored because it needs two things this repo does not and cannot ship:
    /// a locally built PCem core (`just pcem-core`), and IBM's copyrighted BIOS
    /// ROMs under `<system dir>/pcem/roms/`. With both in place:
    ///
    ///   DEMARC_CORE_DIR=external/pcem/build-lr/src \
    ///       cargo test boots_an_ibm_xt -- --ignored --nocapture
    ///
    /// With no disks attached the 1981 BIOS falls through to the Cassette
    /// BASIC in ROM, so a successful boot ends on a screen that cannot be
    /// mistaken for anything else.
    #[test]
    #[ignore = "needs a locally built pcem core and IBM BIOS ROMs"]
    fn boots_an_ibm_xt_to_rom_basic() {
        let roms = system_dir().join("pcem").join("roms");
        assert!(
            roms.join("ibmxt").is_dir(),
            "no BIOS ROMs at {} - see testdata/pc/ibmxt.cfg",
            roms.display()
        );
        let font = super::screen::load_font(&roms);

        let cfg = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pc")
            .join("ibmxt.cfg");

        let args = Args::parse_from(["demarc"]);
        let systems = NewSys::new(&args);
        let mut loaded = systems
            .load_file(&cfg, &HashMap::new())
            .expect("failed to load the XT config");
        assert_eq!(loaded.system.name(), "PC");

        // Both milestones of a real boot: the BIOS sizing memory, then BASIC.
        let mut post_line: Option<String> = None;
        let mut basic_at: Option<usize> = None;
        let mut last = Vec::new();
        let mut frame = 0;
        let deadline = Instant::now() + TIMEOUT;

        while frame < BOOT_FRAME_LIMIT {
            // The core runs on its own thread and `run` only picks up a frame
            // it has already finished, so a `false` means "nothing ready yet",
            // not "no more frames".
            if !loaded.backend.run() {
                assert!(Instant::now() < deadline, "the core stopped producing frames");
                std::thread::yield_now();
                continue;
            }
            frame += 1;
            if frame % DECODE_EVERY != 0 {
                continue;
            }

            let mut text = Vec::new();
            loaded.backend.with_frame(&mut |width, height, pixels| {
                if width >= 640 && height >= 200 {
                    text = super::screen::decode(width, height, pixels, &font);
                }
            });
            if text.is_empty() {
                continue;
            }

            // Keep the newest count rather than the first: the POST writes the
            // running total to the same line as it climbs.
            if let Some(line) = text.iter().find(|l| l.ends_with("KB OK")) {
                post_line = Some(line.clone());
            }
            // The banner alone is not enough: it is printed a character at a
            // time, so sampling during it catches a half-drawn line. "Ok" is
            // BASIC's prompt, and only appears once the interpreter is up and
            // everything above it has been written.
            if text[0].contains("IBM Personal Computer Basic") && text.iter().any(|l| l == "Ok") {
                basic_at = Some(frame);
                last = text;
                break;
            }
            last = text;
        }

        let screen = last.join("\n");
        let frame = basic_at.unwrap_or_else(|| {
            panic!("never reached ROM BASIC in {BOOT_FRAME_LIMIT} frames. Last screen:\n{screen}")
        });
        println!("POST: {}", post_line.as_deref().unwrap_or("(never seen)"));
        println!("booted to ROM BASIC at frame {frame}:\n{screen}");

        // The POST memory count has to agree with `mem_size = 640` in the
        // config — the one number the config and the emulated hardware have to
        // negotiate before the machine will come up at all.
        assert_eq!(
            post_line.as_deref().map(str::trim),
            Some("640 KB OK"),
            "unexpected POST memory count"
        );

        assert!(
            last[1].contains("Copyright IBM Corp 1981"),
            "second line was {:?}",
            last[1]
        );
        assert!(
            last[2].ends_with("Bytes free"),
            "third line was {:?}",
            last[2]
        );
    }

    /// Second Reality's own SETUP screen wants Enter before it starts. Sending
    /// one every second is easier to trust than trying to spot the screen: the
    /// keystrokes before it land at the DOS prompt, where they do nothing.
    const ENTER_EVERY: usize = 60;

    /// Enough for the POST, the FreeDOS boot, JEMMEX, and the demo loading a
    /// megabyte off C: — it reaches the SETUP screen around frame 2000.
    const DEMO_FRAME_LIMIT: usize = 9000;

    /// Wall-clock ceiling for the whole run, so a wedged core fails rather than
    /// hangs. A debug build is a long way off 60 fps here.
    const DEMO_TIMEOUT: Duration = Duration::from_secs(600);

    /// Frames to watch once the demo has switched to its 320x200 mode.
    const ANIMATION_FRAMES: usize = 300;

    /// Boot DOS off a floppy image and run Second Reality from a hard disc.
    ///
    /// Ignored for the same reasons as the XT test — it needs `just pcem-core`
    /// and an AMI 486 BIOS under `<system dir>/pcem/roms/` — plus the ET4000
    /// video BIOS:
    ///
    ///   DEMARC_CORE_DIR=external/pcem/build-lr/src \
    ///       cargo test runs_second_reality -- --ignored --nocapture
    ///
    /// Unlike the XT, there is no text to read at the end: this is a graphics
    /// demo. What it asserts instead is that the machine leaves text mode for
    /// the 320x200 the demo runs in, and that what arrives after that is a
    /// moving picture rather than one frame held still — which cannot happen
    /// without the BIOS, DOS, the hard disc and the video card all working.
    #[test]
    #[ignore = "needs a locally built pcem core and an AMI 486 BIOS"]
    fn runs_second_reality_from_a_dos_hard_disc() {
        let roms = system_dir().join("pcem").join("roms");
        assert!(
            roms.join("ami486").is_dir() && roms.join("et4000.bin").is_file(),
            "no AMI 486 / ET4000 ROMs at {} - see testdata/pc/2ndreality.cfg",
            roms.display()
        );

        let cfg = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("pc")
            .join("2ndreality.cfg");

        let args = Args::parse_from(["demarc"]);
        let systems = NewSys::new(&args);
        let mut loaded = systems
            .load_file(&cfg, &HashMap::new())
            .expect("failed to load the Second Reality config");
        assert_eq!(loaded.system.name(), "PC");

        // Every video mode the machine passes through, in order. The POST and
        // the SETUP screen are 80x25 text; the demo proper is 320x200.
        let mut modes: Vec<(usize, usize, usize)> = Vec::new();
        let mut graphics_at = None;
        let mut seen_text_mode = false;
        let mut frame = 0;
        let deadline = Instant::now() + DEMO_TIMEOUT;

        while frame < DEMO_FRAME_LIMIT {
            if !loaded.backend.run() {
                assert!(Instant::now() < deadline, "the core stopped producing frames");
                std::thread::yield_now();
                continue;
            }
            frame += 1;

            let (width, height) = loaded.backend.get_frame_size();
            if modes.last().map(|&(_, w, h)| (w, h)) != Some((width, height)) {
                modes.push((frame, width, height));
            }
            // PCem starts out at the CGA-ish 656x200 it uses before any card
            // has set a mode, so "not 80x25" is not enough on its own: wait
            // for the ET4000's 720x400 text mode, and only then for the demo
            // to switch away from it.
            if height >= 350 {
                seen_text_mode = true;
            } else if seen_text_mode {
                graphics_at = Some(frame);
                break;
            }
            if frame % ENTER_EVERY == 0 {
                loaded.backend.send_keys(&[(0, RETROK_RETURN)]);
            }
        }

        let modes_seen = modes
            .iter()
            .map(|(at, w, h)| format!("{w}x{h} at {at}"))
            .collect::<Vec<_>>()
            .join(", ");
        let graphics_at = graphics_at.unwrap_or_else(|| {
            panic!("never left text mode in {DEMO_FRAME_LIMIT} frames. Modes: {modes_seen}")
        });
        println!("modes: {modes_seen}");
        println!("in graphics mode at frame {graphics_at}");

        // A demo that has started is a picture that keeps changing. One held
        // frame - a crash back to DOS, or a mode set with nothing behind it -
        // gives a single hash and a near-empty palette.
        let mut hashes = std::collections::HashSet::new();
        let mut colours = std::collections::HashSet::new();
        let mut watched = 0;
        while watched < ANIMATION_FRAMES {
            if !loaded.backend.run() {
                assert!(Instant::now() < deadline, "the core stopped producing frames");
                std::thread::yield_now();
                continue;
            }
            watched += 1;
            hashes.insert(loaded.backend.frame_hash());
            loaded.backend.with_frame(&mut |_, _, pixels| {
                colours.extend(pixels.iter().copied());
            });
        }
        println!(
            "{} distinct frames and {} colours over {ANIMATION_FRAMES} frames",
            hashes.len(),
            colours.len()
        );

        assert!(
            hashes.len() > ANIMATION_FRAMES / 10,
            "only {} distinct frames in {ANIMATION_FRAMES} - the picture is not moving",
            hashes.len()
        );
        assert!(
            colours.len() > 16,
            "only {} distinct colours - the screen is effectively blank",
            colours.len()
        );
    }
}

/// Reading the emulated screen back as text.
///
/// A pixel hash would say the frame changed, not that the machine booted, and
/// it would go stale on any cosmetic change in PCem. Decoding the text instead
/// lets the test assert on what the BIOS actually printed.
#[cfg(test)]
mod screen {
    use std::collections::HashMap;
    use std::path::Path;

    /// An 80x25 text mode is 640x200 pixels, but the card blits its overscan
    /// border too, putting the first character cell 8 pixels in and 4 down.
    const BORDER_X: usize = 8;
    const BORDER_Y: usize = 4;
    pub const COLS: usize = 80;
    pub const ROWS: usize = 25;
    const CELL: usize = 8;

    /// Anything brighter than this in any channel counts as foreground. CGA
    /// text uses a fixed 16-colour palette with nothing near the middle, so
    /// there is no borderline case to get wrong.
    const LIT: u8 = 96;

    /// Maps an 8x8 glyph bitmap back to its character code.
    pub type Font = HashMap<[u8; 8], u8>;

    /// Load the 8x8 CGA font PCem renders text modes with.
    ///
    /// `loadfont(.., FONT_MDA)` in PCem's video.c reads `mda.rom` as four
    /// 2048-byte blocks — the two halves of the 8x14 MDA font, then the thin
    /// and the thick 8x8 CGA fonts. The last block is the one CGA text uses.
    pub fn load_font(roms: &Path) -> Font {
        let rom = std::fs::read(roms.join("mda.rom")).expect("mda.rom (the CGA font) is missing");
        assert!(rom.len() >= 8192, "mda.rom is too short to hold four fonts");

        let mut font = Font::new();
        for ch in 0..=255u8 {
            let off = 6144 + usize::from(ch) * 8;
            let glyph: [u8; 8] = rom[off..off + 8].try_into().unwrap();
            // Codes can share a bitmap — NUL and space are both blank — and
            // first one wins, so a blank cell reads back as NUL, not space.
            // decode() turns both into a space anyway.
            font.entry(glyph).or_insert(ch);
        }
        font
    }

    /// Decode a CGA text-mode frame into its 25 lines of text.
    ///
    /// Cells are matched against the font both as-is and inverted, so the
    /// black-on-white function key bar along the bottom of the BASIC screen
    /// reads like everything else. A cell matching neither becomes `?`.
    pub fn decode(width: usize, height: usize, pixels: &[u32], font: &Font) -> Vec<String> {
        assert!(
            width >= BORDER_X + COLS * CELL && height >= BORDER_Y + ROWS * CELL,
            "frame is {width}x{height}, too small for an 80x25 text mode"
        );

        (0..ROWS)
            .map(|row| {
                let mut line = String::with_capacity(COLS);
                for col in 0..COLS {
                    let mut glyph = [0u8; 8];
                    for (y, bits) in glyph.iter_mut().enumerate() {
                        for x in 0..CELL {
                            let i = (BORDER_Y + row * CELL + y) * width + BORDER_X + col * CELL + x;
                            let [r, g, b, _] = pixels[i].to_ne_bytes();
                            if r > LIT || g > LIT || b > LIT {
                                *bits |= 0x80 >> x;
                            }
                        }
                    }
                    let inverse = glyph.map(|b| !b);
                    match font.get(&glyph).or_else(|| font.get(&inverse)) {
                        Some(&ch) if (0x20..0x7f).contains(&ch) => line.push(ch as char),
                        // Control codes and the line-drawing half of the set:
                        // present, but not what this test reads.
                        Some(_) => line.push(' '),
                        None => line.push('?'),
                    }
                }
                line.trim_end().to_string()
            })
            .collect()
    }
}
