use super::*;
use sha2::{Digest, Sha256};

fn intro(name: &str) -> Vec<u8> {
    fs::read(
        Path::new(env!("DEMARC_ROOT"))
            .join("testdata/crinkler")
            .join(name),
    )
    .unwrap()
}

fn exports(version: u32) -> Exports {
    Exports::load(
        &Path::new(env!("DEMARC_ROOT")).join("system/win/crinkler/exports.txt"),
        version,
    )
    .unwrap()
}

fn sha(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn versions_are_read_from_the_header() {
    assert_eq!(version(&intro("eisenerz.exe")), Some(22));
    assert_eq!(version(&intro("cassini.exe")), Some(23));
    assert_eq!(version(b"MZ not a crinkler exe at all, just text padding it out to length ................................................................................................................................................................................................................................"), None);
}

/// The unpacked code is byte for byte what the stub leaves in memory under
/// wine (dumped at its final `ret`), for the 4k header, the 1k one and a
/// 1.1 compatibility header.
#[test]
fn depacker_output_matches_a_real_run() {
    for (name, len, hash) in [
        (
            "eisenerz.exe",
            0x2375,
            "e75e7355c80dba72ca906b272589b76578df116907718d9ad3fae2d1016d0330",
        ),
        (
            "quattro.exe",
            0x2cc1,
            "2afdd81fa865cbaf61020e5b4ff16612e35a07f5c63b98cc221d8b63e18970b3",
        ),
        (
            "cassini.exe",
            0x618,
            "395df6867f2040b31de823514d0efb21b06b0ae702facac8dc498edd30b3d441",
        ),
        (
            "organix.exe",
            0x2652,
            "3523240ead2ed0030775b2b768c68f4dd640512a0759c5fdc19085e5a3f45d3b",
        ),
    ] {
        let code = decompress(&intro(name)).unwrap();
        assert_eq!(code.len(), len, "{name}");
        assert_eq!(sha(&code), hash, "{name}");
    }
}

/// Eisenerz's tiny import: wine's user32 also exports
/// ShellHandwritingHandleDelegatedInput, which lands on GetDC's slot and sorts
/// after it. The 2.2 list doesn't have it, so the slot is GetDC again.
#[test]
fn tiny_import_slots_resolve_to_the_names_crinkler_picked() {
    let data = intro("eisenerz.exe");
    let code = decompress(&data).unwrap();
    let found = find_imports(&code, &data, &exports(22)).unwrap();
    let names: Vec<_> = found
        .imports
        .iter()
        .map(|i| format!("{}!{}", i.dll, i.name))
        .collect();
    for want in [
        "user32!GetDC",
        "opengl32!wglCreateContext",
        "gdi32!SwapBuffers",
        "kernel32!ExitProcess",
        "winmm!waveOutWrite",
    ] {
        assert!(names.contains(&want.to_owned()), "{want} not in {names:?}");
    }
    assert_eq!(names.len(), 21);
}

#[test]
fn hash_list_imports_resolve_in_order() {
    let data = intro("quattro.exe");
    let code = decompress(&data).unwrap();
    let found = find_imports(&code, &data, &exports(22)).unwrap();
    assert_eq!(found.imports[0].dll, "kernel32");
    assert!(found.imports.windows(2).all(|w| w[1].slot == w[0].slot + 4));
    assert!(found.imports.iter().any(|i| i.dll == "opengl32"));
}

/// The rebuilt file is an ordinary PE: the unpacked code where Crinkler put
/// it, and an import directory naming what the code calls.
#[test]
fn rebuilt_exe_imports_by_name() {
    let data = intro("eisenerz.exe");
    let pe = rebuild(&data, &exports(22)).unwrap();
    assert_eq!(&pe[0..2], b"MZ");
    let at = u32_at(&pe, 0x3c) as usize;
    assert_eq!(&pe[at..at + 4], b"PE\0\0");
    assert!(pe.windows(6).any(|w| w == b"GetDC\0"));
    assert!(pe.windows(13).any(|w| w == b"opengl32.dll\0"));
    let code = decompress(&data).unwrap();
    // The import loop is a jump now; the rest of the code is as unpacked.
    let start = pe.len() - align(code.len() as u32, 0x200) as usize;
    assert_eq!(pe[start + 0x18], 0xe9);
    assert_eq!(&pe[start + 0x82..start + code.len()], &code[0x82..]);
}

/// 1.x headers overlap the hash list, so some hashes are dummies that match
/// nothing and that the code never calls through.
#[test]
fn dummy_hashes_of_old_headers_are_skipped() {
    let data = intro("organix.exe");
    assert_eq!(version(&data), Some(11));
    let code = decompress(&data).unwrap();
    let found = find_imports(&code, &data, &exports(11)).unwrap();
    let names: Vec<_> = found.imports.iter().map(|i| i.name.as_str()).collect();
    assert!(names.contains(&"CreateSemaphoreA") && names.contains(&"StretchDIBits"));
    assert_eq!(found.imports[0].slot, 0x430014);
}

/// Cassini sets its pixel format through gdi32, where the opengl32 proxy
/// never sees it; the rebuilt exe imports it from opengl32 instead.
#[test]
fn gdi32_pixel_formats_go_through_opengl32() {
    let pe = rebuild(&intro("cassini.exe"), &exports(23)).unwrap();
    assert!(pe.windows(18).any(|w| w == b"wglSetPixelFormat\0"));
    assert!(!pe.windows(10).any(|w| w == b"gdi32.dll\0"));
}

/// A maximized window created at 0x0 gets the screen's size from a wrapper,
/// which needs GetSystemMetrics.
#[test]
fn create_window_is_wrapped() {
    let pe = rebuild(&intro("organix.exe"), &exports(11)).unwrap();
    assert!(pe.windows(17).any(|w| w == b"GetSystemMetrics\0"));
    // test byte [esp+13h], 1 opens the wrapper.
    assert!(pe.windows(5).any(|w| w == [0xf6, 0x44, 0x24, 0x13, 0x01]));
}
