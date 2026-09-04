use super::*;

fn slot(name: &str) -> Option<u32> {
    disk_slot(Path::new(name).file_stem()?.to_str()?).map(DiskSlot::number)
}

fn sorted(names: &[&str]) -> Vec<String> {
    let mut paths: Vec<PathBuf> = names.iter().map(PathBuf::from).collect();
    sort_disks(&mut paths);
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn strips_lha_filename_comments() {
    assert_eq!(
        strip_lha_comment("dcs-nons.exe%00from _Shape (@b112b.mtalo.ton.tut.fi)"),
        "dcs-nons.exe"
    );
    assert_eq!(
        strip_lha_comment("eld-sotd/eld-sotd.exe\0mail@host"),
        "eld-sotd/eld-sotd.exe"
    );
    assert_eq!(strip_lha_comment("plain.exe"), "plain.exe");
    assert_eq!(strip_lha_comment("%00only a comment"), "");
}

#[test]
fn numbers_disks() {
    assert_eq!(slot("disk3.adf"), Some(3));
    assert_eq!(slot("45degreesA.adf"), Some(1));
    assert_eq!(slot("3witches.dsk"), None);
    assert_eq!(slot("game_B.DMS"), Some(2));
    assert_eq!(slot("GOA.dsk"), None);
    assert_eq!(slot("disk2_[cr].adf"), Some(2));
    assert_eq!(slot("shadow1992.adf"), None);
}

#[test]
fn fills_first_slot_from_unnumbered_disks() {
    assert_eq!(
        sorted(&["extra.adf", "Space_disk2.adf", "space.adf"]),
        ["space.adf", "Space_disk2.adf", "extra.adf"]
    );
}

#[test]
fn prefers_digits_over_letters() {
    assert_eq!(
        sorted(&["disk_A.adf", "disk_1.adf", "disk_B.adf", "disk_2.adf"]),
        ["disk_1.adf", "disk_2.adf", "disk_A.adf", "disk_B.adf"]
    );
}

#[test]
fn sorts_plainly_when_every_disk_claims_the_same_slot() {
    // Nothing here numbers a disk, so the slot rules would only shuffle the
    // names: a digit outranking a letter, or a hole being filled.
    assert_eq!(
        sorted(&["red_A.adf", "blue_1.adf"]),
        ["blue_1.adf", "red_A.adf"]
    );
    assert_eq!(
        sorted(&["intro3.adf", "credits3.adf", "main3.adf"]),
        ["credits3.adf", "intro3.adf", "main3.adf"]
    );
}

#[test]
fn keeps_every_disk() {
    assert_eq!(
        sorted(&["b.adf", "a.adf"]),
        ["a.adf", "b.adf"],
        "nothing to sort by, but no disk may be dropped"
    );
}

#[test]
fn strips_windows_verbatim_prefix() {
    // Cores can't open these; system_dir() must hand out the plain form.
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\C:\demarc\system\amiga")),
        PathBuf::from(r"C:\demarc\system\amiga")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share\roms")),
        PathBuf::from(r"\\server\share\roms")
    );
    // Anything without the prefix, and every unix path, is left alone.
    assert_eq!(
        strip_verbatim_prefix(Path::new(r"C:\demarc")),
        PathBuf::from(r"C:\demarc")
    );
    assert_eq!(
        strip_verbatim_prefix(Path::new("/home/sasq/system/amiga")),
        PathBuf::from("/home/sasq/system/amiga")
    );
}
