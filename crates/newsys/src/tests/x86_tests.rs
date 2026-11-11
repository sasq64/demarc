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

#[test]
fn real_mode_uses_segments_and_16_bit_operands() {
    let mut cpu = Cpu::new();
    cpu.real = true;
    cpu.seg = [0x100, 0x100, 0x100, 0x200, 0, 0];
    cpu.r[ESP] = 0xfffe;
    // mov ax, 0x1234; mov [0x10], ax; call +0; pop bx
    let code = [0xb8, 0x34, 0x12, 0xa3, 0x10, 0x00, 0xe8, 0, 0, 0x5b];
    cpu.mem.load(0x1000, &code);
    cpu.eip = 0x1000;
    cpu.run_until(0x1000 + code.len() as u32, 100).unwrap();
    assert_eq!(cpu.mem.read(0x2010, Size::W), 0x1234);
    assert_eq!(cpu.r[EBX], 9);
    assert_eq!(cpu.r[ESP], 0xfffe);
}

/// Run `code` at 0x1000 over `data` at 0x10000 with `run_until`, which takes
/// the fused loops, and again one `step` at a time; both must agree.
fn fused_matches_stepping(code: &[u8], data: &[u8], regs: [u32; 8], stop: u32) {
    let setup = || {
        let mut cpu = Cpu::new();
        cpu.mem.load(0x1000, code);
        cpu.mem.load(0x10000, data);
        cpu.r = regs;
        cpu.eip = 0x1000;
        cpu
    };
    assert!(setup().fused(u64::MAX).unwrap().is_some(), "not fused");
    let mut fused = setup();
    let steps = fused.run_until(stop, 1 << 30).unwrap();
    let mut stepped = setup();
    let mut n = 0;
    while stepped.eip != stop {
        stepped.step().unwrap();
        n += 1;
    }
    let flags = |c: &Cpu| (c.cf, c.pf, c.zf, c.sf, c.of);
    assert_eq!(steps, n);
    assert_eq!(fused.r, stepped.r);
    assert_eq!(flags(&fused), flags(&stepped));
    assert_eq!(
        fused.mem.slice(0x7000, 0x1000),
        stepped.mem.slice(0x7000, 0x1000)
    );
}

fn noise(len: usize, values: u32) -> Vec<u8> {
    let mut x = 12345u32;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            ((x >> 16) % values) as u8
        })
        .collect()
}

#[test]
fn fused_1k_context_match_is_exact() {
    let mut code = vec![0; 0x8a];
    code[..0x1a].copy_from_slice(&[
        0x60, 0x8a, 0x06, 0xd2, 0xe8, 0x32, 0x07, 0x75, 0x6b, 0x4e, 0x4f, 0x31, 0xc9, 0xbd, 0x64,
        0, 0, 0, 0xd0, 0xeb, 0x72, 0xeb, 0x75, 0xf1, 0xeb, 0x5a,
    ]);
    code[0x72..0x79].copy_from_slice(&[0xeb, 0x8c, 0x61, 0x75, 0x0e, 0xeb, 0x11]);
    code[0x85..0x8a].copy_from_slice(&[0x46, 0x39, 0xf7, 0x7f, 0xe8]);
    for (values, mask, shift) in [(2, 0b1011, 1), (256, 0b1, 0), (3, 0b110, 9)] {
        let mut regs = [0; 8];
        (regs[ESI], regs[EDI], regs[EBX], regs[ECX], regs[ESP]) =
            (0x10010, 0x100c0, mask, shift, 0x8000);
        fused_matches_stepping(&code, &noise(256, values), regs, 0x108a);
    }
}

#[test]
fn fused_4k_context_hash_is_exact() {
    let imul = [
        0x32, 0x07, 0x6b, 0xc0, 0x6f, 0x02, 0x87, 0x10, 0, 0, 0, 0x48, 0x4f, 0x00, 0xd2, 0x72,
        0xef, 0x75, 0xf9,
    ];
    let rol = [
        0x32, 0x07, 0xc1, 0xc0, 0x09, 0x02, 0x07, 0x48, 0x4f, 0x00, 0xd2, 0x72, 0xf3, 0x75, 0xf9,
    ];
    for code in [&imul[..], &rol[..]] {
        for mask in [0x01, 0xa5, 0xff, 0x40] {
            let mut regs = [0; 8];
            (regs[EAX], regs[EDI], regs[EDX], regs[ESP]) =
                (0x1234_5678, 0x10080, 0xabcd_0000 | mask, 0x8000);
            fused_matches_stepping(code, &noise(256, 256), regs, 0x1000 + code.len() as u32);
        }
    }
}

#[test]
fn rep_stos_fills_and_skips_untouched_zero_pages() {
    let mut cpu = Cpu::new();
    cpu.mem.write(0x3ffe, Size::D, 0xffff_ffff);
    // mov edi, 0x2001; mov ecx, 0x1801; mov ax, 0x0102; rep stosw
    let code = [
        0xbf, 0x01, 0x20, 0, 0, 0xb9, 0x01, 0x18, 0, 0, 0x66, 0xb8, 0x02, 0x01, 0xf3, 0x66, 0xab,
    ];
    cpu.mem.load(0x1000, &code);
    cpu.eip = 0x1000;
    cpu.run_until(0x1000 + code.len() as u32, 100).unwrap();
    assert_eq!(cpu.r[EDI], 0x2001 + 0x3002);
    assert_eq!(cpu.r[ECX], 0);
    assert_eq!(cpu.mem.read(0x2000, Size::D), 0x0201_0200);
    assert_eq!(cpu.mem.read(0x3ffe, Size::D), 0x0201_0201);
    assert_eq!(cpu.mem.read(0x5002, Size::D), 0x0000_0001);

    // xor eax, eax; mov edi, 0x100000; mov ecx, 0x100000; rep stosd
    let code = [
        0x31, 0xc0, 0xbf, 0, 0, 0x10, 0, 0xb9, 0, 0, 0x10, 0, 0xf3, 0xab,
    ];
    cpu.mem.load(0x1000, &code);
    cpu.eip = 0x1000;
    cpu.run_until(0x1000 + code.len() as u32, 100).unwrap();
    assert_eq!(cpu.mem.last_nonzero(0x100000), None);
}
