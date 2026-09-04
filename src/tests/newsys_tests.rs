use clap::Parser;
use tracing_subscriber::{EnvFilter, fmt};

use super::*;

fn init_tracing() {
    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

fn test_load(path: &Path, name: &str) -> WorkFile {
    let args = Args::parse_from(["demarc"]);
    let s = NewSys::new(&args);

    let mut result = s.load_file(path, &HashMap::new(), None).unwrap();
    println!("{:?}", result.work_file.get_all_meta());
    assert_eq!(result.system.name(), name);
    result.backend.run();
    result.work_file
}

/// Build a stored zip holding one file.
fn zip_with(path: &Path, entry: &str, contents: &[u8]) {
    use std::io::Write;
    let mut zw = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored);
    zw.start_file(entry, opts).unwrap();
    zw.write_all(contents).unwrap();
    zw.finish().unwrap();
}

/// The half of loading that runs off the main thread: a release packed
/// inside another archive — as scene releases routinely are — comes out as
/// a temp directory holding the innermost files, with the meta it was
/// handed still on it.
#[test]
fn unpacking_reaches_into_a_double_packed_release() {
    let dir = tempfile::tempdir().unwrap();
    let inner = dir.path().join("inner.zip");
    zip_with(&inner, "demo.adf", b"the release itself");
    let outer = dir.path().join("outer.zip");
    zip_with(&outer, "inner.zip", &fs::read(&inner).unwrap());

    let meta = HashMap::from([("latency".to_string(), "2".to_string())]);
    let wf = unpack_release(&outer, &meta).unwrap();

    assert!(wf.is_temporary(), "unpacking never writes beside the archive");
    assert_eq!(
        fs::read(wf.path.join("demo.adf")).unwrap(),
        b"the release itself",
        "the inner archive is unpacked too"
    );
    assert_eq!(wf.get_meta_or("latency", ""), "2");
}

/// A file that is not an archive is left exactly where it is — nothing is
/// copied, so a local release the user pointed at stays untouched.
#[test]
fn unpacking_leaves_a_plain_file_alone() {
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("demo.adf");
    fs::write(&game, b"the release itself").unwrap();

    let wf = unpack_release(&game, &HashMap::new()).unwrap();

    assert_eq!(wf.path, game);
    assert!(!wf.is_temporary());
}

/// An override writes the config file a DOS release was packed without and
/// names which of its programs to start, both found inside the release
/// whatever case the archive spelled them in.
#[test]
fn an_override_patches_the_release_and_picks_what_starts_it() {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("inside");
    fs::create_dir_all(&release).unwrap();
    fs::write(release.join("INSTALL.EXE"), b"MZ").unwrap();
    fs::write(release.join("INSIDE.EXE"), b"MZ").unwrap();

    let over = Override {
        boot_file: Some("inside.exe"),
        meta: HashMap::from([("dosbox_pure_cycles", "max")]),
        patches: vec![Patch {
            target: "SOUND.CFG",
            offset: None,
            data: "AAEC",
            info: "GUS 0x240",
        }],
        ..Default::default()
    };

    let mut wf = WorkFile::new(release.clone());
    apply_override(&mut wf, &over).unwrap();

    assert_eq!(wf.get_meta_or("dosbox_pure_cycles", ""), "max");
    assert!(
        wf.is_temporary() && wf.path.starts_with(wf.temp_dir().unwrap()),
        "patching works on a copy, never the user's own files"
    );
    assert!(wf.path.ends_with("INSIDE.EXE"), "started {:?}", wf.path);
    assert_eq!(
        Path::new(&wf.get_meta_or(RELEASE_DIR, "")),
        wf.path.parent().unwrap(),
        "the release the program was picked out of is left behind"
    );
    // Written next to the program, which is the directory DOSBox mounts.
    let cfg = wf.path.parent().unwrap().join("SOUND.CFG");
    assert_eq!(fs::read(cfg).unwrap(), [0, 1, 2]);
    assert!(
        release.join("INSIDE.EXE").exists() && !release.join("SOUND.CFG").exists(),
        "the release itself is untouched"
    );
}

/// `fast = true` writes the accelerated Amiga configuration, and does it
/// first, so an entry that also names one of those options keeps it.
#[test]
fn fast_sets_the_amiga_configuration_the_entry_can_still_amend() {
    let over = Override {
        fast: true,
        meta: HashMap::from([("amiberry_cpu_model", "68040")]),
        ..Default::default()
    };

    let mut wf = WorkFile::new(PathBuf::from("demo.lha"));
    apply_override(&mut wf, &over).unwrap();

    assert_eq!(wf.get_meta_or("amiberry_model", ""), "A1200");
    assert_eq!(wf.get_meta_or("amiberry_fastmem_size", ""), "8");
    assert_eq!(wf.get_meta_or("amiberry_z3mem_size", ""), "128");
    assert_eq!(wf.get_meta_or("amiberry_jit", ""), "enabled");
    assert_eq!(wf.get_meta_or("amiberry_cpu_speed", ""), "max");
    assert_eq!(wf.get_meta_or("puae_cpu_model", ""), "68030");
    assert_eq!(
        wf.get_meta_or("amiberry_cpu_model", ""),
        "68040",
        "the entry's own option is written after the fast configuration"
    );
}

/// A patch with an offset writes into the file it names rather than
/// replacing it, and one naming a file that isn't there creates it.
#[test]
fn a_patch_may_write_into_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("demo");
    fs::create_dir_all(&release).unwrap();
    fs::write(release.join("demo.exe"), b"0123456789").unwrap();

    let patches = [
        Patch {
            target: "demo.exe",
            offset: Some(4),
            data: "AAEC",
            ..Default::default()
        },
        Patch {
            target: "new.cfg",
            offset: Some(2),
            data: "AAEC",
            ..Default::default()
        },
    ];
    let mut wf = WorkFile::new(release);
    apply_patches(&mut wf, &patches).unwrap();

    assert_eq!(
        fs::read(wf.path.join("demo.exe")).unwrap(),
        b"0123\0\x01\x02789",
        "the three bytes at the offset, and nothing else"
    );
    // Nothing there to write into, so the gap in front is zero filled.
    assert_eq!(fs::read(wf.path.join("new.cfg")).unwrap(), [0, 0, 0, 1, 2]);
}

#[test]
fn test_c64() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("testdata").join("c64");

    test_load(&testdata.join("quantum.prg"), "C64");
    test_load(&testdata.join("DEMO060A.rar"), "C64");
    test_load(&testdata.join("Maniacs of Noise Logo.t64.gz"), "C64");
    test_load(&testdata.join("cd"), "C64");
    assert!(!testdata.join("cd").join("demo.m3u").exists());
    test_load(&testdata.join("cd/The_Violators-CD_s1.d64"), "C64");
    test_load(&testdata.join("Skaaneland.zip"), "C64");
}

#[test]
fn test_amiga() {
    init_tracing();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("testdata").join("amiga");
    test_load(&testdata.join("desert"), "Amiga");
    assert!(!testdata.join("desert").join("demo.m3u").exists());
    test_load(&testdata.join("desert").join("disk1.adf"), "Amiga");
    test_load(&testdata.join("desert.zip"), "Amiga");
    test_load(&testdata.join("rebels.adf"), "Amiga");
    test_load(&testdata.join("o2-intro"), "Amiga");

    // A plain executable is booted from a generated startup-sequence on a
    // stock A500, not through WHDLoad.
    let work_file = test_load(&testdata.join("o2-intro").join("o2intro"), "Amiga");
    assert!(work_file.get_meta_or("puae_use_whdload", "") == "disabled");
    assert!(work_file.get_meta_or("puae_model", "") == "A500");

    // The generated drive carries a LIBS: of its own. Kickstart has only
    // some of the system libraries in ROM, and a demo that opens one of the
    // others exits with no message at all when OpenLibrary() comes back
    // empty. This one boots a 1.3 A500, whose skeleton is `system/ami13`;
    // the AGA drive gets the larger `system/amihdd` with `lowlevel.library`
    // (the keyboard and joypad) in it, which no 1.3 machine ever had.
    assert!(
        work_file
            .path
            .join("libs")
            .join("mathtrans.library")
            .exists(),
        "generated drive has no LIBS:"
    );

    // A WHDLoad install (a `.slave` next to the data) turns WHDLoad on and
    // needs an A1200.
    let work_file = test_load(&testdata.join("nexus7"), "Amiga");
    assert!(work_file.get_meta_or("puae_use_whdload", "") == "enabled");
    assert!(work_file.get_meta_or("puae_model", "") == "A1200");
}
/// A bare music file has no system of its own, so it falls through every
/// other system to [`MusicSystem`] — both on its own and as the only
/// playable thing in a directory.
#[test]
fn test_music() {
    let dir = std::env::temp_dir().join("newsys_music_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let song = dir.join("tune.mod");
    crate::music_emu::write_test_mod(&song);

    test_load(&song, "Music");
    test_load(&dir, "Music");
}

/// ST pictures reach [`ImageSystem`] both by extension and, since they are
/// as often named after the release as `.pi1`, by content. A screenshot
/// next to one doesn't win over it.
#[test]
fn test_degas_images() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("testdata").join("degas");
    test_load(&testdata.join("FUSE.PI1"), "Images");
    test_load(&testdata.join("BOLEK3.PC1"), "Images");
    test_load(&testdata.join("ST4EVER.NEO"), "Images");
    test_load(&testdata.join("ATARIMAN.CA1"), "Images");
    test_load(&testdata.join("EXO7.KID"), "Images");

    let dir = std::env::temp_dir().join("newsys_degas_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    // Named so the walk reaches the screenshot first, and with the
    // extension stripped so only the sniff can find the picture.
    fs::copy(testdata.join("FUSE.PI1"), dir.join("zz-picture")).unwrap();
    fs::write(dir.join("aa-shot.png"), b"not really a png").unwrap();

    let work_file = test_load(&dir, "Images");
    assert!(
        work_file.path.ends_with("zz-picture"),
        "picked {:?} over the DEGAS picture",
        work_file.path
    );
}

#[test]
fn test_psx() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let testdata = root.join("testdata").join("psx");
    test_load(&testdata.join("paradox").join("pdx-051.psx"), "PSX");
    test_load(&testdata.join("monophobia"), "PSX");
    // A bare data track with no cue beside it, named `.bin` like any other
    // dump, is recognised from the disc's own contents.
    test_load(&testdata.join("thisispsx"), "PSX");
}
