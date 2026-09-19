use super::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use clap::Parser;

use crate::Args;
use crate::newsys::NewSys;
use crate::system_dir;

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    write_bytes(dir, name, body.as_bytes())
}

fn write_bytes(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).unwrap();
    path
}

/// `.cfg` is a name half the world uses, so the sniff has to lean on the
/// `machine =` key rather than the extension.
#[test]
fn tells_a_machine_config_from_any_other_cfg() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};

    let cfg = write(
        dir.path(),
        "486.cfg",
        "machine = ami486\ncpu_family = i486dx\nmem_size = 16384\ngfxcard = tgui9440\n",
    );
    assert!(sys.can_load(&cfg));

    // Section headers and spacing vary between hand-written configs.
    let spaced = write(dir.path(), "xt.cfg", "\n[Machine]\n  machine=ibmxt86\n");
    assert!(sys.can_load(&spaced));

    // Some other emulator's settings file that happens to end in .cfg.
    let other = write(dir.path(), "other.cfg", "fullscreen = 1\nscale = 2\n");
    assert!(!sys.can_load(&other));

    // A `machine` key with nothing after it configures no machine.
    let empty = write(dir.path(), "empty.cfg", "machine = \n");
    assert!(!sys.can_load(&empty));

    // A key that merely starts with "machine" is not the machine key.
    let lookalike = write(dir.path(), "look.cfg", "machine_name = foo\n");
    assert!(!sys.can_load(&lookalike));
}

/// An MZ header is not enough on its own: every Windows program has one
/// too, and DOSBox can run none of them.
#[test]
fn tells_a_dos_program_from_a_windows_one() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};

    // A DOS executable: `MZ`, and e_lfanew left as it comes.
    let mut dos = vec![0u8; 0x80];
    dos[..2].copy_from_slice(b"MZ");
    let dos = write_bytes(dir.path(), "demo.exe", &dos);
    assert!(sys.can_load(&dos));
    assert_eq!(core_for(&dos), CORE_NAME_DOSBOX);

    // The same header in front of a PE image is a Windows program: never a
    // DOS one, and never this system's to start - see `super::windows`.
    let mut win = vec![0u8; 0x100];
    win[..2].copy_from_slice(b"MZ");
    win[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    win[0x80..0x84].copy_from_slice(b"PE\0\0");
    let win = write_bytes(dir.path(), "setup32.exe", &win);
    assert_eq!(exe_kind(&win), ExeKind::Windows);
    assert!(!is_dos_program(&win));
    assert!(!sys.can_load(&win));

    // A DOS extender (LE/LX behind the stub) is how the demos were built.
    let mut dos4gw = vec![0u8; 0x100];
    dos4gw[..2].copy_from_slice(b"MZ");
    dos4gw[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    dos4gw[0x80..0x82].copy_from_slice(b"LE");
    let dos4gw = write_bytes(dir.path(), "dos4gw.exe", &dos4gw);
    assert!(sys.can_load(&dos4gw));

    // An offset pointing past the end of the file is a DOS program with a
    // field it never set, not a Windows one whose image we failed to find.
    let mut stub = vec![0u8; 0x80];
    stub[..2].copy_from_slice(b"MZ");
    stub[0x3c..0x40].copy_from_slice(&0x1000u32.to_le_bytes());
    let stub = write_bytes(dir.path(), "stub.exe", &stub);
    assert!(is_dos_program(&stub));

    // A 64K intro packs the two headers into one: `e_lfanew` points at
    // 0x0c, so the PE header's own fields make up the rest of the DOS
    // header. Well inside it, and still a Windows program.
    let mut tiny = vec![0u8; 0x1000];
    tiny[..2].copy_from_slice(b"MZ");
    tiny[0x0c..0x10].copy_from_slice(b"PE\0\0");
    tiny[0x3c..0x40].copy_from_slice(&0x0cu32.to_le_bytes());
    let tiny = write_bytes(dir.path(), "intro64k.exe", &tiny);
    assert_eq!(exe_kind(&tiny), ExeKind::Windows);
    assert!(!is_dos_program(&tiny));

    // Not an executable at all.
    let text = write(dir.path(), "notes.exe", "just a text file\n");
    assert!(!sys.can_load(&text));

    // `.com` has no header, only a size DOS could load into one segment.
    let com = write_bytes(dir.path(), "tiny.com", &[0xcd, 0x20]);
    assert!(sys.can_load(&com));
    let huge = write_bytes(dir.path(), "big.com", &vec![0u8; 0x1_0000]);
    assert!(!sys.can_load(&huge));
    let empty = write_bytes(dir.path(), "nothing.com", &[]);
    assert!(!sys.can_load(&empty));
}

/// The two cores split by content, not by system: a machine config drives
/// 86Box, everything else runs on DOSBox's own DOS.
#[test]
fn routes_each_kind_of_content_to_its_core() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = write(dir.path(), "486.cfg", "machine = ami486\n");
    assert_eq!(core_for(&cfg), CORE_NAME_86BOX);
    assert_eq!(core_for(Path::new("demo.exe")), CORE_NAME_DOSBOX);
    assert_eq!(core_for(Path::new("go.bat")), CORE_NAME_DOSBOX);
    assert_eq!(core_for(Path::new("tiny.com")), CORE_NAME_DOSBOX);
}

/// What a release directory holds is rarely one program, and the walk
/// order says nothing about which of them the release is.
#[test]
fn picks_what_the_release_means_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};

    let release = dir.path().join("crystal");
    fs::create_dir_all(&release).unwrap();
    let mut exe = vec![0u8; 0x80];
    exe[..2].copy_from_slice(b"MZ");
    // Reached first by the walk, and the last thing anyone wants to run.
    write_bytes(&release, "install.exe", &exe);
    write_bytes(&release, "crystal.exe", &exe);
    write_bytes(&release, "zzsetup.exe", &exe);

    let found = sys.pick_target(&release).unwrap().unwrap();
    assert!(
        found.ends_with("crystal.exe"),
        "picked {found:?} out of the release"
    );

    // A machine config beside the programs describes the whole machine, so
    // it wins - and takes the release to 86Box rather than DOSBox.
    write(&release, "crystal.cfg", "machine = ami486\n");
    let found = sys.pick_target(&release).unwrap().unwrap();
    assert!(found.ends_with("crystal.cfg"), "picked {found:?}");
    assert_eq!(core_for(&found), CORE_NAME_86BOX);
}

/// A `.bat` beside the program is as often a wrapper printing the .NFO as
/// it is the way in, and the program starts the same without it.
#[test]
fn starts_the_program_rather_than_the_batch_file_beside_it() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};
    let mut exe = vec![0u8; 0x80];
    exe[..2].copy_from_slice(b"MZ");

    // Named so that neither of them wins on the release name.
    let release = dir.path().join("release");
    fs::create_dir_all(&release).unwrap();
    write(&release, "go.bat", "@echo off\r\ndemo.exe\r\n");
    write_bytes(&release, "trip.exe", &exe);
    let found = sys.pick_target(&release).unwrap().unwrap();
    assert!(found.ends_with("trip.exe"), "picked {found:?}");
}

/// A DOS program was linked under a name DOS could type. Anything longer
/// than 8.3, or with a character DOS never had, was named by whatever
/// handled the release afterwards.
#[test]
fn prefers_a_program_with_a_name_dos_could_have_held() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};
    let mut exe = vec![0u8; 0x80];
    exe[..2].copy_from_slice(b"MZ");

    // Named so that neither wins on the release name, and the long one
    // written first so the walk reaches it before the program.
    let release = dir.path().join("release");
    fs::create_dir_all(&release).unwrap();
    write_bytes(&release, "read me first.exe", &exe);
    write_bytes(&release, "trip.exe", &exe);
    let found = sys.pick_target(&release).unwrap().unwrap();
    assert!(found.ends_with("trip.exe"), "picked {found:?}");

    // It only breaks a tie: a program named after the release is still
    // what the release is, however that name looks.
    let named = dir.path().join("crystal demo");
    fs::create_dir_all(&named).unwrap();
    write_bytes(&named, "crystal demo.exe", &exe);
    write_bytes(&named, "trip.exe", &exe);
    let found = sys.pick_target(&named).unwrap().unwrap();
    assert!(found.ends_with("crystal demo.exe"), "picked {found:?}");

    // And an installer stays an installer whatever its name looks like.
    let installer = dir.path().join("installer");
    fs::create_dir_all(&installer).unwrap();
    write_bytes(&installer, "install.exe", &exe);
    write_bytes(&installer, "the whole demo.exe", &exe);
    let found = sys.pick_target(&installer).unwrap().unwrap();
    assert!(found.ends_with("the whole demo.exe"), "picked {found:?}");
}

/// What counts as a name DOS itself could hold.
#[test]
fn knows_an_8_3_name_from_a_longer_one() {
    for name in ["demo.exe", "GO.BAT", "a.c", "trip_95.exe", "cw$dpmi.exe"] {
        assert!(is_simple_name(Path::new(name)), "{name} is an 8.3 name");
    }
    for name in [
        // Nine characters in the stem, four in the extension.
        "crystals1.exe",
        "seconddemo.exe",
        "demo.html",
        // Characters DOS never had in a name.
        "read me.exe",
        "démo.exe",
        "demo\u{2b50}.exe",
        // One dot, and on the right side of the name.
        "demo",
        ".exe",
        "readme.txt.exe",
    ] {
        assert!(!is_simple_name(Path::new(name)), "{name} is not 8.3");
    }
    assert!(is_simple_name(Path::new("crystal.exe")));
}

/// The extender is shipped beside the program that loads it, so it is the
/// one `.exe` in a release that never starts anything.
#[test]
fn passes_over_an_extender_shipped_beside_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};
    let mut exe = vec![0u8; 0x80];
    exe[..2].copy_from_slice(b"MZ");

    let release = dir.path().join("release");
    fs::create_dir_all(&release).unwrap();
    // Named so that neither wins on the release name, and written first so
    // that the walk reaches the extender before the demo.
    write_bytes(&release, "DOS4GW.EXE", &exe);
    write_bytes(&release, "trip.exe", &exe);
    let found = sys.pick_target(&release).unwrap().unwrap();
    assert!(found.ends_with("trip.exe"), "picked {found:?}");

    // On its own it is still all there is to start.
    let bare = dir.path().join("bare");
    fs::create_dir_all(&bare).unwrap();
    write_bytes(&bare, "dos4gw.exe", &exe);
    let found = sys.pick_target(&bare).unwrap().unwrap();
    assert!(found.ends_with("dos4gw.exe"), "picked {found:?}");
}

/// `dos4gw=true` says the release was packed without the extender it was
/// linked against, which means writing into the release — so it has to be
/// a copy of one.
#[test]
fn puts_the_extender_beside_a_release_asking_for_one() {
    let dir = tempfile::tempdir().unwrap();
    let sys = DosSystem {};
    let mut exe = vec![0u8; 0x80];
    exe[..2].copy_from_slice(b"MZ");

    let release = dir.path().join("crystal");
    fs::create_dir_all(release.join("data")).unwrap();
    write_bytes(&release, "crystal.exe", &exe);
    write(&release.join("data"), "tune.mod", "not really a module");

    // Without the option the release is started where it lies.
    let mut plain = WorkFile::new(release.clone());
    assert!(sys.load(&mut plain).unwrap());
    assert_eq!(plain.path, release.join("crystal.exe"));
    assert!(!plain.is_temporary());

    let meta = HashMap::from([(META_DOS4GW.to_string(), "true".to_string())]);
    let mut wf = WorkFile::new_with_meta(release.clone(), meta);
    assert!(sys.load(&mut wf).unwrap());

    // The whole release came along, data files and all, since that
    // directory is what DOSBox mounts as C:.
    assert!(wf.is_temporary(), "{:?} is not a copy", wf.path);
    assert!(wf.path.ends_with("crystal.exe"), "{:?}", wf.path);
    let copied = wf.path.parent().unwrap();
    assert!(copied.join("data").join("tune.mod").is_file());

    // Whatever happened, it did not happen in the user's own files.
    assert!(!release.join(DOS4GW_EXE).exists());

    // The extender itself is not ours to ship, so only check it landed
    // when there is one in the system dir to land.
    if dos4gw_source().is_file() {
        assert!(
            copied.join(DOS4GW_EXE).is_file(),
            "no extender in {copied:?}"
        );
    }
}

/// A release that packed its own extender keeps it: it is the version the
/// program was built against, and ours would overwrite it.
#[test]
fn keeps_an_extender_the_release_brought_itself() {
    let dir = tempfile::tempdir().unwrap();
    let source = write(dir.path(), "dos4gw.exe", "ours");

    let release = dir.path().join("release");
    fs::create_dir_all(&release).unwrap();
    // Lowercase here, uppercase in the copy we would make: the same file
    // to DOS, two files on this filesystem.
    write(&release, "dos4gw.exe", "theirs");
    let program = write(&release, "demo.exe", "MZ");

    place_extender(&WorkFile::new(program), &source).unwrap();
    assert_eq!(
        fs::read_to_string(release.join("dos4gw.exe")).unwrap(),
        "theirs"
    );
    assert_eq!(fs::read_dir(&release).unwrap().count(), 2);

    // With none there, the copy is made under the name DOS uses.
    let empty = dir.path().join("empty");
    fs::create_dir_all(&empty).unwrap();
    let program = write(&empty, "demo.exe", "MZ");
    place_extender(&WorkFile::new(program), &source).unwrap();
    assert_eq!(fs::read_to_string(empty.join(DOS4GW_EXE)).unwrap(), "ours");
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
/// a locally built 86Box core (`just 86box-core`), and IBM's copyrighted BIOS
/// ROMs under `<system dir>/86box/roms/`. With both in place:
///
///   DEMARC_CORE_DIR=external/86box/build-lr/src \
///       cargo test boots_an_ibm_xt -- --ignored --nocapture
///
/// With no disks attached the BIOS should fall through to the Cassette BASIC
/// in ROM, so a successful boot ends on a screen that cannot be mistaken for
/// anything else.
///
/// NOT PASSING on 86Box as it stands: `testdata/pc/ibmxt.cfg` gets through the
/// POST memory count and then resets to a blank screen instead of entering
/// BASIC, over and over. The config still needs work.
#[test]
#[ignore = "needs a locally built 86box core and IBM BIOS ROMs"]
fn boots_an_ibm_xt_to_rom_basic() {
    let roms = system_dir().join("86box").join("roms");
    assert!(
        roms.join("machines").join("ibmxt86").is_dir(),
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
        .load_file(&cfg, &HashMap::new(), None)
        .expect("failed to load the XT config");
    assert_eq!(loaded.system.name(), "MS/DOS");

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
            assert!(
                Instant::now() < deadline,
                "the core stopped producing frames"
            );
            std::thread::yield_now();
            continue;
        }
        frame += 1;
        if frame % DECODE_EVERY != 0 {
            continue;
        }

        let mut text = Vec::new();
        loaded.backend.with_frame(&mut |width, height, pixels| {
            if width == 640 && height == 200 {
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
