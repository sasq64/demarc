//! Hot loops of known unpacker stubs, run natively instead of instruction by
//! instruction. Each leaves registers, flags and memory exactly as the
//! interpreter would, and reports how many instructions it stands for.

use super::*;

/// Any byte, in a code pattern.
const ANY: u16 = 0x100;

/// The 1k header's context match: for every earlier position, compare the
/// bytes its context mask selects. Starts at its `pusha`.
const MATCH_1K: &[u16] = &[
    0x60, // pusha
    0x8a, 0x06, // mov al,[esi]
    0xd2, 0xe8, // shr al,cl
    0x32, 0x07, // xor al,[edi]
    0x75, 0x6b, // jnz popa
    0x4e, // dec esi
    0x4f, // dec edi
    0x31, 0xc9, // xor ecx,ecx
    0xbd, ANY, ANY, ANY, ANY, // mov ebp,imm
    0xd0, 0xeb, // shr bl,1
    0x72, 0xeb, // jc mov
    0x75, 0xf1, // jnz dec
    0xeb, 0x5a, // jmp popa
];
const MATCH_1K_IMM: u32 = 14;
const MATCH_1K_LOOP: &[u16] = &[
    0xeb, 0x8c, // jmp pusha
    0x61, // popa
    0x75, 0x0e, // jnz next
];
const MATCH_1K_NEXT: &[u16] = &[
    0x46, // inc esi
    0x39, 0xf7, // cmp edi,esi
    0x7f, 0xe8, // jg jmp
];

/// The 4k headers' context hash: fold in the bytes the context mask selects.
/// Starts at its `xor`.
const HASH_4K: &[u16] = &[
    0x32, 0x07, // xor al,[edi]
    0x6b, 0xc0, ANY, // imul eax,eax,imm8
    0x02, 0x87, ANY, ANY, ANY, ANY,  // add al,[edi+disp32]
    0x48, // dec eax
    0x4f, // dec edi
    0x00, 0xd2, // add dl,dl
    0x72, 0xef, // jc xor
    0x75, 0xf9, // jnz dec edi
];
/// The same in Crinkler 1.1's header.
const HASH_4K_11: &[u16] = &[
    0x32, 0x07, // xor al,[edi]
    0xc1, 0xc0, ANY, // rol eax,imm8
    0x02, 0x07, // add al,[edi]
    0x48, // dec eax
    0x4f, // dec edi
    0x00, 0xd2, // add dl,dl
    0x72, 0xf3, // jc xor
    0x75, 0xf9, // jnz dec edi
];

impl Cpu {
    fn code_is(&self, at: u32, pattern: &[u16]) -> bool {
        (0..pattern.len() as u32).all(|i| {
            let p = pattern[i as usize];
            p == ANY || self.mem.read8(at + i) as u16 == p
        })
    }

    /// Run the fused loop starting at `eip`, if there is one, for about
    /// `budget` instructions at most.
    pub(super) fn fused(&mut self, budget: u64) -> Result<Option<u64>> {
        if self.real {
            return Ok(None);
        }
        match self.mem.read8(self.eip) {
            0x60 => self.match_1k(budget),
            0x32 => Ok(self.hash_4k()),
            _ => Ok(None),
        }
    }

    fn match_1k(&mut self, budget: u64) -> Result<Option<u64>> {
        let start = self.eip;
        if !(self.code_is(start, MATCH_1K)
            && self.code_is(start + 0x72, MATCH_1K_LOOP)
            && self.code_is(start + 0x85, MATCH_1K_NEXT))
        {
            return Ok(None);
        }
        // `popa` puts back every register the compare loop changes, so only
        // its flags and the stack the last `pusha` wrote outlive it.
        let mut pushed = None;
        let mut n = 0;
        while n < budget {
            pushed = Some(self.r);
            n += 1;
            let (mut esi, mut edi, mut bl) = (self.r[ESI], self.r[EDI], self.r[EBX] & 0xff);
            let mut count = self.r[ECX] & 31;
            let matched = 'compare: loop {
                let a = self.mem.read8(esi) as u32;
                let a = if count >= 8 { 0 } else { a >> count };
                let b = self.mem.read8(edi) as u32;
                n += 4;
                if a != b {
                    self.alu(6, a, b, Size::B);
                    break false;
                }
                loop {
                    (esi, edi, count) = (esi.wrapping_sub(1), edi.wrapping_sub(1), 0);
                    let old = bl;
                    bl >>= 1;
                    n += 6;
                    if old & 1 != 0 {
                        continue 'compare;
                    }
                    n += 1;
                    if bl == 0 {
                        n += 1;
                        self.shift(5, old, 1, Size::B)?;
                        break 'compare true;
                    }
                }
            };
            n += 2;
            if matched {
                self.write_pusha(pushed);
                self.eip = start + 0x77;
                return Ok(Some(n));
            }
            self.r[ESI] = self.inc_dec(self.r[ESI], Size::D, false);
            self.alu(7, self.r[EDI], self.r[ESI], Size::D);
            n += 3;
            if !self.cond(0xf) {
                self.write_pusha(pushed);
                self.eip = start + 0x8a;
                return Ok(Some(n));
            }
            n += 1;
        }
        self.write_pusha(pushed);
        self.eip = start;
        Ok(Some(n))
    }

    fn hash_4k(&mut self) -> Option<u64> {
        let start = self.eip;
        let (rol, disp) = if self.code_is(start, HASH_4K) {
            (false, self.mem.read(start + 7, Size::D))
        } else if self.code_is(start, HASH_4K_11) {
            (true, 0)
        } else {
            return None;
        };
        let imm = self.mem.read8(start + 4);
        let (mut eax, mut edi, mut dl) = (self.r[EAX], self.r[EDI], self.r[EDX] & 0xff);
        let mut n = 0;
        // Only the last `add dl,dl` decides the flags left behind.
        let last = 'hash: loop {
            eax ^= self.mem.read8(edi) as u32;
            eax = if rol {
                eax.rotate_left(imm as u32 & 31)
            } else {
                (eax as i32).wrapping_mul(imm as i8 as i32) as u32
            };
            let al = (eax as u8).wrapping_add(self.mem.read8(edi.wrapping_add(disp)));
            eax = (eax & !0xff | al as u32).wrapping_sub(1);
            n += 4;
            loop {
                edi = edi.wrapping_sub(1);
                let old = dl;
                dl = (dl << 1) & 0xff;
                n += 3;
                if old & 0x80 != 0 {
                    continue 'hash;
                }
                n += 1;
                if dl == 0 {
                    break 'hash old;
                }
            }
        };
        self.r[EAX] = eax;
        self.r[EDI] = edi;
        self.r[EDX] = self.r[EDX] & !0xff | dl;
        self.alu(0, last, last, Size::B);
        self.eip = start + if rol { 15 } else { 19 };
        Some(n)
    }

    /// Leave the stack as a `pusha` of `regs` would have.
    fn write_pusha(&mut self, regs: Option<[u32; 8]>) {
        let Some(regs) = regs else { return };
        let now = std::mem::replace(&mut self.r, regs);
        self.pusha(Size::D);
        self.r = now;
    }
}
