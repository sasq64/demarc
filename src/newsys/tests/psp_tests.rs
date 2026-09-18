use super::*;
use std::fs;

fn elf(e_type: u16, machine: u8) -> Vec<u8> {
    let mut h = vec![0u8; 52];
    h[..6].copy_from_slice(b"\x7fELF\x01\x01");
    h[16..18].copy_from_slice(&e_type.to_le_bytes());
    h[18] = machine;
    h
}

/// The kxploit layout of Suicide Barbie: the wrapped PBP alone in `%__SCE__`,
/// the bare ELF in `__SCE__` beside the data it loads.
#[test]
fn picks_the_eboot_beside_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let stub = dir.path().join("%__SCE__Demo");
    let real = dir.path().join("__SCE__Demo");
    fs::create_dir_all(&stub).unwrap();
    fs::create_dir_all(real.join("Data")).unwrap();
    fs::write(stub.join("EBOOT.PBP"), b"\0PBP\0\0\x01\0").unwrap();
    fs::write(real.join("EBOOT.PBP"), elf(2, 8)).unwrap();
    // A PRX module is a MIPS ELF too, and has data beside it.
    fs::write(real.join("Data/module.prx"), elf(0xffa0, 8)).unwrap();
    fs::write(real.join("Data/file"), b"x").unwrap();

    let mut file = WorkFile::new(dir.path());
    assert!(PspSystem {}.load(&mut file).unwrap());
    let stick = PathBuf::from(file.get_meta_or("save_dir", ""));
    let game = stick.join("PSP/GAME/__SCE__Demo");
    assert_eq!(file.path, game.join("EBOOT.PBP"));
    assert!(game.join("Data/file").is_file());
    // The release itself is left alone.
    assert!(real.join("EBOOT.PBP").is_file());
}

#[test]
fn ignores_non_mips_elf() {
    let dir = tempfile::tempdir().unwrap();
    let x86 = dir.path().join("demo");
    fs::write(&x86, elf(2, 3)).unwrap();
    assert!(!is_psp_exe(&x86));
    assert!(!PspSystem {}.load(&mut WorkFile::new(dir.path())).unwrap());
}

#[test]
#[ignore = "needs the Suicide Barbie release unpacked in barbie/"]
fn suicide_barbie_runs() {
    let core_path = crate::libloader::get_libretro(CORE_NAME).unwrap();
    // PPSSPP writes its settings under the system dir.
    let system_dir = tempfile::tempdir().unwrap();
    let mut file = WorkFile::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("barbie"));
    assert!(PspSystem {}.load(&mut file).unwrap());
    let mut emu = crate::retro_emu::RetroCoreDirect::new(
        &core_path,
        system_dir.path(),
        Some(&file.path),
        file.get_all_meta(),
    )
    .unwrap();
    // Paced: PPSSPP boots on a thread of its own, and tearing the core down
    // before that finishes crashes it.
    for _ in 0..600 {
        emu.run();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    emu.with_frame(|w, h, frame| {
        assert!(w > 0 && h > 0, "no frame produced");
        let distinct = frame.iter().collect::<std::collections::HashSet<_>>().len();
        assert!(distinct > 16, "frame looks blank: only {distinct} colours");
    });
}
