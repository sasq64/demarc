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
    for (src, _, size) in ROMS {
        let rom = vec![src.len() as u8; 0x80000.max(size)];
        zip.start_file(format!("{ROM_DIR}{src}"), opts).unwrap();
        zip.write_all(&encrypt(&rom, &key)).unwrap();
    }
    zip.finish().unwrap();

    let dest = dir.path().join("amiga");
    assert_eq!(install_kickstarts(&apk, &dest).unwrap(), 4);
    let kick13 = std::fs::read(dest.join("kick34005.A500")).unwrap();
    assert_eq!(kick13, vec!["amiga-os-130.rom".len() as u8; 0x40000]);
    assert_eq!(install_kickstarts(&apk, &dest).unwrap(), 0, "already there");
}
