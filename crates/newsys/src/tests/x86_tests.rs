use super::*;

fn run(code: &[u8]) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.mem.load(0x1000, code);
    cpu.r[ESP] = 0x8000;
    cpu.eip = 0x1000;
    cpu.run_until(0x1000 + code.len() as u32, 1000).unwrap();
    cpu
}

#[test]
fn add_and_sub_set_carry_and_overflow() {
    // mov eax, 0x7fffffff; add eax, 1
    let cpu = run(&[0xb8, 0xff, 0xff, 0xff, 0x7f, 0x83, 0xc0, 0x01]);
    assert_eq!(cpu.r[EAX], 0x8000_0000);
    assert!(cpu.of && !cpu.cf && cpu.sf && !cpu.zf);
    // xor eax, eax; sub eax, 1
    let cpu = run(&[0x31, 0xc0, 0x83, 0xe8, 0x01]);
    assert_eq!(cpu.r[EAX], 0xffff_ffff);
    assert!(cpu.cf && !cpu.of);
}

#[test]
fn parity_reflects_the_low_byte() {
    // mov al, 3; or al, 0  (two bits set: even parity)
    let cpu = run(&[0xb0, 0x03, 0x0c, 0x00]);
    assert!(cpu.pf);
    // mov al, 7; or al, 0
    let cpu = run(&[0xb0, 0x07, 0x0c, 0x00]);
    assert!(!cpu.pf);
}

#[test]
fn rcl_shifts_through_carry() {
    // stc; mov byte [0x2000], 0x80; rcl byte [0x2000], 1
    let cpu = run(&[
        0xf9, 0xc6, 0x05, 0, 0x20, 0, 0, 0x80, 0xd0, 0x15, 0, 0x20, 0, 0,
    ]);
    assert_eq!(cpu.mem.read8(0x2000), 1);
    assert!(cpu.cf);
}

#[test]
fn mul_and_div_use_edx_eax() {
    // mov eax, 0x10000; mov ecx, 0x10000; mul ecx; mov ecx, 3; div ecx
    let cpu = run(&[
        0xb8, 0, 0, 1, 0, 0xb9, 0, 0, 1, 0, 0xf7, 0xe1, 0xb9, 3, 0, 0, 0, 0xf7, 0xf1,
    ]);
    assert_eq!(cpu.r[EAX], 0x5555_5555);
    assert_eq!(cpu.r[EDX], 1);
}

#[test]
fn pusha_and_popa_round_trip() {
    // mov ebx, 5; pusha; xor ebx, ebx; popa
    let cpu = run(&[0xbb, 5, 0, 0, 0, 0x60, 0x31, 0xdb, 0x61]);
    assert_eq!(cpu.r[EBX], 5);
    assert_eq!(cpu.r[ESP], 0x8000);
}

#[test]
fn bt_with_a_register_offset_reaches_past_the_operand() {
    // mov byte [0x2005], 0x04; mov ebp, 42; bt [0x2000], ebp
    let mut cpu = Cpu::new();
    cpu.mem.write8(0x2005, 0x04);
    cpu.mem.load(
        0x1000,
        &[0xbd, 42, 0, 0, 0, 0x0f, 0xa3, 0x2d, 0, 0x20, 0, 0],
    );
    cpu.eip = 0x1000;
    cpu.run_until(0x100c, 10).unwrap();
    assert!(cpu.cf);
}

#[test]
fn crc32_is_castagnoli() {
    // The standard check value of CRC-32C over "123456789".
    let crc = b"123456789".iter().fold(!0, |c, b| crc32c_byte(c, *b));
    assert_eq!(!crc, 0xe306_9283);
}

#[test]
fn unknown_opcode_stops_with_an_error() {
    let mut cpu = Cpu::new();
    cpu.mem.load(0x1000, &[0xf4]);
    cpu.eip = 0x1000;
    assert!(cpu.step().is_err());
}
