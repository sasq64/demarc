use super::*;

#[test]
fn catches_the_program_a_com_writes_and_starts() {
    #[rustfmt::skip]
    let com = [
        0xb4, 0x3c, 0xba, 0x21, 0x01, 0x31, 0xc9, 0xcd, 0x21, // create C:\T.EXE
        0x89, 0xc3, 0xb4, 0x40, 0xb9, 0x04, 0x00, 0xba, 0x2a, 0x01, 0xcd, 0x21, // write 4
        0xb4, 0x3e, 0xcd, 0x21, // close
        0xb8, 0x00, 0x4b, 0xba, 0x2e, 0x01, 0xcd, 0x21, // exec t.exe
        b'C', b':', b'\\', b'T', b'.', b'E', b'X', b'E', 0,
        b'M', b'Z', 0x90, 0,
        b't', b'.', b'e', b'x', b'e', 0,
    ];
    assert_eq!(run(&com).unwrap(), b"MZ\x90\0");
}

#[test]
fn exiting_without_exec_is_an_error() {
    // ret to the int 20h at the start of the PSP
    assert!(run(&[0xc3]).is_err());
}

/// `DEMARC_COM=<file.com> cargo test -p newsys dropper_from_env -- --ignored`
#[test]
#[ignore]
fn dropper_from_env() {
    let path = std::env::var("DEMARC_COM").unwrap();
    let exe = run(&fs::read(&path).unwrap()).unwrap();
    fs::write(Path::new(&path).with_extension("exe"), &exe).unwrap();
    println!("{} bytes", exe.len());
}
