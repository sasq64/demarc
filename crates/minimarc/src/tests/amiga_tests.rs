use super::*;
use std::io::Write;

fn encrypt(rom: &[u8], key: &[u8]) -> Vec<u8> {
    let mut out = CLOANTO_HEADER.to_vec();
    out.extend(rom.iter().zip(key.iter().cycle()).map(|(b, k)| b ^ k));
    out
}

#[test]
fn a_cloanto_rom_decrypts_to_its_size() {
    let key = b"secret key";
    let rom: Vec<u8> = (0..32u8).collect();
    let doubled = [rom.clone(), rom.clone()].concat();
    assert_eq!(decrypt(&encrypt(&doubled, key), key, 32), Some(rom));
}

#[test]
fn a_plain_rom_is_refused() {
    assert_eq!(decrypt(&[0u8; 64], b"key", 32), None);
}

#[test]
fn kickstarts_are_written_under_their_names() {
    let dir = tempfile::tempdir().unwrap();
    let apk = dir.path().join("forever.apk");
    let key = b"the key".to_vec();
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&apk).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file(format!("{ROM_DIR}rom.key"), opts).unwrap();
    zip.write_all(&key).unwrap();
    for Rom { cloanto, size, .. } in ROMS {
        let rom = vec![cloanto.len() as u8; 0x80000.max(size)];
        zip.start_file(format!("{ROM_DIR}{cloanto}"), opts).unwrap();
        zip.write_all(&encrypt(&rom, &key)).unwrap();
    }
    zip.finish().unwrap();

    let dest = dir.path().join("amiga");
    assert_eq!(install_kickstarts(&apk, &dest).unwrap(), 4);
    let kick13 = std::fs::read(dest.join("kick34005.A500")).unwrap();
    assert_eq!(kick13, vec!["amiga-os-130.rom".len() as u8; 0x40000]);
    assert_eq!(install_kickstarts(&apk, &dest).unwrap(), 0, "already there");
}

fn fake_roms() -> [Rom; 2] {
    let rom = |name, version, crcs| Rom {
        cloanto: "",
        name,
        size: 0,
        version,
        crcs,
    };
    static A: OnceLock<[u32; 2]> = OnceLock::new();
    static B: OnceLock<[u32; 1]> = OnceLock::new();
    let a = A.get_or_init(|| [crc32fast::hash(b"rom a"), crc32fast::hash(b"alt a")]);
    let b = B.get_or_init(|| [crc32fast::hash(b"rom b")]);
    [rom("kick.a", "1.0", a), rom("kick.b", "2.0", b)]
}

fn versions(roms: Vec<&Rom>) -> Vec<&str> {
    roms.iter().map(|rom| rom.version).collect()
}

#[test]
fn a_rom_is_missing_when_absent_or_wrong() {
    let dir = tempfile::tempdir().unwrap();
    let roms = fake_roms();
    assert_eq!(versions(missing_in(dir.path(), &roms)), ["1.0", "2.0"]);
    std::fs::write(dir.path().join("kick.a"), b"alt a").unwrap();
    std::fs::write(dir.path().join("kick.b"), b"not b").unwrap();
    assert_eq!(versions(missing_in(dir.path(), &roms)), ["2.0"]);
}

#[test]
fn roms_are_imported_by_checksum_whatever_their_name() {
    let from = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    std::fs::write(from.path().join("My ROM.bin"), b"rom b").unwrap();
    std::fs::write(from.path().join("other.rom"), b"junk").unwrap();
    let roms = fake_roms();
    assert_eq!(import_in(from.path(), dest.path(), &roms).unwrap(), 1);
    assert_eq!(std::fs::read(dest.path().join("kick.b")).unwrap(), b"rom b");
    assert_eq!(versions(missing_in(dest.path(), &roms)), ["1.0"]);
}

#[test]
fn a_doubled_rom_is_checked_by_its_half() {
    let half: Vec<u8> = (0..0x40000u32).map(|i| i as u8).collect();
    let doubled = [half.clone(), half.clone()].concat();
    assert_eq!(rom_crc(&doubled), crc32fast::hash(&half));
}
