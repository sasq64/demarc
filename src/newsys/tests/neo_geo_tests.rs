use super::*;

/// The loose files a devkit leaves behind have to come back as a disc the
/// same detection accepts, or the release would only load the once.
#[test]
fn builds_a_disc_from_loose_files() {
    let dir = std::env::temp_dir().join("demarc_neocd_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("IPL.TXT"), "TEST.PRG,0,0\r\nFIX.FIX,0,0\r\n\u{1a}").unwrap();
    fs::write(dir.join("Test.prg"), vec![1u8; 90_000]).unwrap();
    fs::write(dir.join("fix.fix"), vec![2u8; 4096]).unwrap();
    // Neither belongs on the disc: a subdirectory the boot ROM can't reach,
    // and a name that isn't 8.3.
    fs::create_dir_all(dir.join("sources")).unwrap();
    fs::write(dir.join("sources/intro.s"), b"; source").unwrap();
    fs::write(dir.join("a much longer name.txt"), b"readme").unwrap();

    let cue = create_neocd_disc(&dir.join("IPL.TXT")).unwrap();
    assert!(is_neogeo_cd_cue(&cue));

    let mut image = DiscImage::open(&cue.with_file_name("disc.iso")).unwrap();
    assert_eq!(image.root_names(), ["FIX.FIX", "IPL.TXT", "TEST.PRG"]);

    // Same contents, same image — a release unpacked to a new temp
    // directory each launch must not rebuild it.
    assert_eq!(create_neocd_disc(&dir.join("IPL.TXT")).unwrap(), cue);
}

/// The boot list is `NAME,BANK,OFFSET` per line, CRLF terminated, and ends
/// with a DOS EOF byte that is not part of the last name.
#[test]
fn reads_the_boot_list() {
    let text = "TEST.PRG,0,0\r\nFIX.FIX,0,0\r\nSOUND9V3.Z80,0,00\r\n\u{1a}";
    assert_eq!(ipl_entries(text), ["TEST.PRG", "FIX.FIX", "SOUND9V3.Z80"]);
}
