use super::*;

fn entry_bytes(path: &str, dir: bool, contents: &[u8]) -> Vec<u8> {
    let name: Vec<u8> = path
        .encode_utf16()
        .chain([0])
        .flat_map(|c| c.to_le_bytes())
        .collect();
    let data_at = 0x4a + name.len();
    let mut e = vec![0; data_at];
    e[0..4].copy_from_slice(&((data_at + contents.len()) as u32).to_le_bytes());
    e[4] = 5;
    e[0xe] = dir as u8;
    e[0x2a..0x2e].copy_from_slice(&(contents.len() as u32).to_le_bytes());
    for (n, at) in [0x4a, 0x4a, 0x4a, 0x4a, data_at].into_iter().enumerate() {
        e[0x36 + n * 4..0x3a + n * 4].copy_from_slice(&(at as u32).to_le_bytes());
    }
    e[0x4a..data_at].copy_from_slice(&name);
    e.extend_from_slice(contents);
    e
}

#[test]
fn files_skips_dirs_and_foreign_roots() {
    let mut section = b"BoxedApp header".to_vec();
    section.extend(entry_bytes("<SystemRoot>", true, b""));
    section.extend(entry_bytes("demo_Data", true, b""));
    section.extend(entry_bytes("demo_Data\\level0", false, b"level data"));
    section.extend(entry_bytes("C:\\boot.ini", false, b"x"));
    section.extend(entry_bytes("demo_Data\\empty", false, b""));
    section.extend([0; 0x60]);
    let found: Vec<_> = files(&section)
        .into_iter()
        .map(|(path, range)| (path, section[range].to_vec()))
        .collect();
    assert_eq!(
        found,
        [
            ("demo_Data/level0".to_owned(), b"level data".to_vec()),
            ("demo_Data/empty".to_owned(), vec![]),
        ]
    );
}
