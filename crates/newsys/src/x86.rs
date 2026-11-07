//! A small 32-bit x86 interpreter over flat, paged memory.
//!
//! Enough of the integer instruction set to run self-contained unpacker stubs
//! (see [`crate::crinkler`]): no protected mode, no FPU, no I/O. An opcode it
//! does not know stops it with an error rather than guessing.

use anyhow::{Result, bail};

const PAGE: u32 = 4096;

/// 4 GB in 4 KB pages, each allocated on first write.
pub struct Memory {
    pages: Vec<Option<Box<[u8; PAGE as usize]>>>,
}

impl Default for Memory {
    fn default() -> Self {
        Self {
            pages: (0..1u32 << 20).map(|_| None).collect(),
        }
    }
}

impl Memory {
    pub fn read8(&self, addr: u32) -> u8 {
        self.pages[(addr / PAGE) as usize]
            .as_ref()
            .map_or(0, |p| p[(addr % PAGE) as usize])
    }

    pub fn write8(&mut self, addr: u32, val: u8) {
        let page =
            self.pages[(addr / PAGE) as usize].get_or_insert_with(|| Box::new([0; PAGE as usize]));
        page[(addr % PAGE) as usize] = val;
    }

    pub fn read(&self, addr: u32, size: Size) -> u32 {
        let off = (addr % PAGE) as usize;
        let n = size.bytes() as usize;
        if off + n <= PAGE as usize {
            let Some(page) = &self.pages[(addr / PAGE) as usize] else {
                return 0;
            };
            let mut b = [0u8; 4];
            b[..n].copy_from_slice(&page[off..off + n]);
            return u32::from_le_bytes(b);
        }
        (0..size.bytes()).fold(0, |v, i| {
            v | (self.read8(addr.wrapping_add(i)) as u32) << (8 * i)
        })
    }

    pub fn write(&mut self, addr: u32, size: Size, val: u32) {
        let off = (addr % PAGE) as usize;
        let n = size.bytes() as usize;
        if off + n <= PAGE as usize {
            let page = self.pages[(addr / PAGE) as usize]
                .get_or_insert_with(|| Box::new([0; PAGE as usize]));
            page[off..off + n].copy_from_slice(&val.to_le_bytes()[..n]);
            return;
        }
        for i in 0..size.bytes() {
            self.write8(addr.wrapping_add(i), (val >> (8 * i)) as u8);
        }
    }

    pub fn load(&mut self, addr: u32, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            self.write8(addr.wrapping_add(i as u32), *b);
        }
    }

    pub fn slice(&self, addr: u32, len: u32) -> Vec<u8> {
        (0..len).map(|i| self.read8(addr.wrapping_add(i))).collect()
    }

    /// The highest address at or above `from` holding a nonzero byte, if any.
    pub fn last_nonzero(&self, from: u32) -> Option<u32> {
        (from / PAGE..1 << 20).rev().find_map(|p| {
            let data = self.pages[p as usize].as_ref()?;
            let base = p * PAGE;
            (0..PAGE)
                .rev()
                .find(|i| data[*i as usize] != 0 && base + i >= from)
                .map(|i| base + i)
        })
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Size {
    B,
    W,
    D,
}

impl Size {
    fn bytes(self) -> u32 {
        match self {
            Size::B => 1,
            Size::W => 2,
            Size::D => 4,
        }
    }
    fn bits(self) -> u32 {
        self.bytes() * 8
    }
    fn mask(self) -> u32 {
        match self {
            Size::B => 0xff,
            Size::W => 0xffff,
            Size::D => 0xffff_ffff,
        }
    }
    fn sign(self) -> u32 {
        1 << (self.bits() - 1)
    }
}

#[derive(Clone, Copy)]
enum Loc {
    Reg(u8),
    Mem(u32),
}

pub const EAX: usize = 0;
pub const ECX: usize = 1;
pub const EDX: usize = 2;
pub const EBX: usize = 3;
pub const ESP: usize = 4;
pub const EBP: usize = 5;
pub const ESI: usize = 6;
pub const EDI: usize = 7;

#[derive(Default)]
pub struct Cpu {
    pub r: [u32; 8],
    pub eip: u32,
    pub cf: bool,
    pub pf: bool,
    pub zf: bool,
    pub sf: bool,
    pub of: bool,
    pub df: bool,
    pub mem: Memory,
}

impl Cpu {
    pub fn new() -> Self {
        Self::default()
    }

    fn fetch8(&mut self) -> u8 {
        let v = self.mem.read8(self.eip);
        self.eip = self.eip.wrapping_add(1);
        v
    }

    fn fetch(&mut self, size: Size) -> u32 {
        let v = self.mem.read(self.eip, size);
        self.eip = self.eip.wrapping_add(size.bytes());
        v
    }

    fn fetch_s8(&mut self) -> u32 {
        self.fetch8() as i8 as u32
    }

    fn reg(&self, n: u8, size: Size) -> u32 {
        let n = n as usize;
        match size {
            Size::B if n < 4 => self.r[n] & 0xff,
            Size::B => (self.r[n - 4] >> 8) & 0xff,
            Size::W => self.r[n] & 0xffff,
            Size::D => self.r[n],
        }
    }

    fn set_reg(&mut self, n: u8, size: Size, val: u32) {
        let n = n as usize;
        match size {
            Size::B if n < 4 => self.r[n] = (self.r[n] & !0xff) | (val & 0xff),
            Size::B => self.r[n - 4] = (self.r[n - 4] & !0xff00) | ((val & 0xff) << 8),
            Size::W => self.r[n] = (self.r[n] & !0xffff) | (val & 0xffff),
            Size::D => self.r[n] = val,
        }
    }

    fn get(&self, loc: Loc, size: Size) -> u32 {
        match loc {
            Loc::Reg(n) => self.reg(n, size),
            Loc::Mem(a) => self.mem.read(a, size),
        }
    }

    fn set(&mut self, loc: Loc, size: Size, val: u32) {
        match loc {
            Loc::Reg(n) => self.set_reg(n, size, val),
            Loc::Mem(a) => self.mem.write(a, size, val),
        }
    }

    pub fn push(&mut self, val: u32) {
        self.r[ESP] = self.r[ESP].wrapping_sub(4);
        self.mem.write(self.r[ESP], Size::D, val);
    }

    pub fn pop(&mut self) -> u32 {
        let v = self.mem.read(self.r[ESP], Size::D);
        self.r[ESP] = self.r[ESP].wrapping_add(4);
        v
    }

    /// Decode a ModRM byte (32-bit addressing): the `reg` field and the r/m operand.
    fn modrm(&mut self) -> (u8, Loc) {
        let m = self.fetch8();
        let (md, reg, rm) = (m >> 6, (m >> 3) & 7, m & 7);
        if md == 3 {
            return (reg, Loc::Reg(rm));
        }
        let mut addr = if rm == 4 {
            let sib = self.fetch8();
            let (scale, index, base) = (sib >> 6, (sib >> 3) & 7, sib & 7);
            let mut a = if index == 4 {
                0
            } else {
                self.r[index as usize] << scale
            };
            if base == 5 && md == 0 {
                a = a.wrapping_add(self.fetch(Size::D));
            } else {
                a = a.wrapping_add(self.r[base as usize]);
            }
            a
        } else if rm == 5 && md == 0 {
            self.fetch(Size::D)
        } else {
            self.r[rm as usize]
        };
        match md {
            1 => addr = addr.wrapping_add(self.fetch_s8()),
            2 => addr = addr.wrapping_add(self.fetch(Size::D)),
            _ => {}
        }
        (reg, Loc::Mem(addr))
    }

    fn set_szp(&mut self, res: u32, size: Size) {
        let res = res & size.mask();
        self.zf = res == 0;
        self.sf = res & size.sign() != 0;
        self.pf = (res as u8).count_ones().is_multiple_of(2);
    }

    /// The eight classic ALU operations, in opcode order.
    fn alu(&mut self, op: u8, a: u32, b: u32, size: Size) -> u32 {
        let (mask, sign) = (size.mask(), size.sign());
        let (a, b) = (a & mask, b & mask);
        let res = match op {
            0 | 2 => {
                let carry = (op == 2 && self.cf) as u64;
                let wide = a as u64 + b as u64 + carry;
                let res = (wide as u32) & mask;
                self.cf = wide > mask as u64;
                self.of = (!(a ^ b) & (a ^ res) & sign) != 0;
                res
            }
            5 | 3 | 7 => {
                let borrow = (op == 3 && self.cf) as u64;
                let res = (a as u64).wrapping_sub(b as u64).wrapping_sub(borrow) as u32 & mask;
                self.cf = (a as u64) < b as u64 + borrow;
                self.of = ((a ^ b) & (a ^ res) & sign) != 0;
                res
            }
            1 | 4 | 6 => {
                self.cf = false;
                self.of = false;
                match op {
                    1 => a | b,
                    4 => a & b,
                    _ => a ^ b,
                }
            }
            _ => unreachable!(),
        };
        self.set_szp(res, size);
        res
    }

    fn inc_dec(&mut self, val: u32, size: Size, dec: bool) -> u32 {
        let cf = self.cf;
        let res = self.alu(if dec { 5 } else { 0 }, val, 1, size);
        self.cf = cf;
        res
    }

    fn shift(&mut self, op: u8, val: u32, count: u32, size: Size) -> Result<u32> {
        let count = count & 31;
        if count == 0 {
            return Ok(val);
        }
        let (bits, mask, sign) = (size.bits(), size.mask(), size.sign());
        let mut v = val & mask;
        match op {
            0 | 1 => {
                for _ in 0..count % bits {
                    v = if op == 0 {
                        ((v << 1) | (v >> (bits - 1))) & mask
                    } else {
                        (v >> 1) | ((v & 1) << (bits - 1))
                    };
                }
                self.cf = if op == 0 { v & 1 != 0 } else { v & sign != 0 };
                self.of = if op == 0 {
                    (v & sign != 0) != self.cf
                } else {
                    ((v ^ (v << 1)) & sign) != 0
                };
            }
            2 | 3 => {
                for _ in 0..count {
                    if op == 2 {
                        let out = v & sign != 0;
                        v = ((v << 1) | self.cf as u32) & mask;
                        self.cf = out;
                    } else {
                        let out = v & 1 != 0;
                        v = (v >> 1) | ((self.cf as u32) << (bits - 1));
                        self.cf = out;
                    }
                }
                self.of = if op == 2 {
                    (v & sign != 0) != self.cf
                } else {
                    ((v ^ (v << 1)) & sign) != 0
                };
            }
            4 | 6 => {
                self.cf = count <= bits && (v >> (bits - count)) & 1 != 0;
                v = if count >= bits {
                    0
                } else {
                    (v << count) & mask
                };
                self.of = (v & sign != 0) != self.cf;
                self.set_szp(v, size);
            }
            5 => {
                self.cf = (v >> (count - 1)) & 1 != 0;
                self.of = val & sign != 0;
                v = if count >= bits { 0 } else { v >> count };
                self.set_szp(v, size);
            }
            7 => {
                let sv = ((v << (32 - bits)) as i32) >> (32 - bits);
                self.cf = (sv >> (count.min(bits) - 1)) & 1 != 0;
                v = (sv >> count.min(31)) as u32 & mask;
                self.of = false;
                self.set_szp(v, size);
            }
            _ => bail!("shift op {op}"),
        }
        Ok(v)
    }

    fn cond(&self, cc: u8) -> bool {
        let r = match cc >> 1 {
            0 => self.of,
            1 => self.cf,
            2 => self.zf,
            3 => self.cf || self.zf,
            4 => self.sf,
            5 => self.pf,
            6 => self.sf != self.of,
            _ => self.zf || self.sf != self.of,
        };
        r != (cc & 1 != 0)
    }

    fn jump(&mut self, offset: u32) {
        self.eip = self.eip.wrapping_add(offset);
    }

    /// Execute one instruction.
    pub fn step(&mut self) -> Result<()> {
        let start = self.eip;
        let mut size = Size::D;
        let mut rep = 0u8;
        let mut op = self.fetch8();
        loop {
            match op {
                0x66 => size = Size::W,
                0xf2 | 0xf3 => rep = op,
                _ => break,
            }
            op = self.fetch8();
        }
        let wide = |op: u8| if op & 1 == 0 { Size::B } else { size };
        match op {
            // ALU r/m,r  r,r/m  al/eax,imm
            0x00..=0x3f if op & 7 < 6 => {
                let alu = op >> 3;
                let sz = wide(op);
                match op & 7 {
                    0..=3 => {
                        let (reg, loc) = self.modrm();
                        let (dst, a, b) = if op & 2 == 0 {
                            (loc, self.get(loc, sz), self.reg(reg, sz))
                        } else {
                            (Loc::Reg(reg), self.reg(reg, sz), self.get(loc, sz))
                        };
                        let res = self.alu(alu, a, b, sz);
                        if alu != 7 {
                            self.set(dst, sz, res);
                        }
                    }
                    _ => {
                        let imm = self.fetch(sz);
                        let res = self.alu(alu, self.reg(0, sz), imm, sz);
                        if alu != 7 {
                            self.set_reg(0, sz, res);
                        }
                    }
                }
            }
            0x40..=0x4f => {
                let n = op & 7;
                let res = self.inc_dec(self.reg(n, size), size, op >= 0x48);
                self.set_reg(n, size, res);
            }
            0x50..=0x57 => self.push(self.r[(op & 7) as usize]),
            0x58..=0x5f => {
                let v = self.pop();
                self.r[(op & 7) as usize] = v;
            }
            0x60 => {
                let sp = self.r[ESP];
                for n in 0..8 {
                    self.push(if n == ESP { sp } else { self.r[n] });
                }
            }
            0x61 => {
                for n in (0..8).rev() {
                    let v = self.pop();
                    if n != ESP {
                        self.r[n] = v;
                    }
                }
            }
            0x68 => {
                let v = self.fetch(Size::D);
                self.push(v);
            }
            0x6a => {
                let v = self.fetch_s8();
                self.push(v);
            }
            0x69 | 0x6b => {
                let (reg, loc) = self.modrm();
                let a = self.get(loc, size);
                let b = if op == 0x69 {
                    self.fetch(size)
                } else {
                    self.fetch_s8()
                };
                self.imul(reg, a, b, size);
            }
            0x70..=0x7f => {
                let off = self.fetch_s8();
                if self.cond(op & 15) {
                    self.jump(off);
                }
            }
            0x80 | 0x81 | 0x83 => {
                let sz = wide(op);
                let (alu, loc) = self.modrm();
                let imm = if op == 0x83 {
                    self.fetch_s8()
                } else {
                    self.fetch(sz)
                };
                let res = self.alu(alu, self.get(loc, sz), imm, sz);
                if alu != 7 {
                    self.set(loc, sz, res);
                }
            }
            0x84 | 0x85 => {
                let sz = wide(op);
                let (reg, loc) = self.modrm();
                self.alu(4, self.get(loc, sz), self.reg(reg, sz), sz);
            }
            0x86 | 0x87 => {
                let sz = wide(op);
                let (reg, loc) = self.modrm();
                let (a, b) = (self.get(loc, sz), self.reg(reg, sz));
                self.set(loc, sz, b);
                self.set_reg(reg, sz, a);
            }
            0x88..=0x8b => {
                let sz = wide(op);
                let (reg, loc) = self.modrm();
                if op & 2 == 0 {
                    self.set(loc, sz, self.reg(reg, sz));
                } else {
                    let v = self.get(loc, sz);
                    self.set_reg(reg, sz, v);
                }
            }
            0x8d => match self.modrm() {
                (reg, Loc::Mem(a)) => self.set_reg(reg, size, a),
                _ => bail!("lea with a register operand at {start:#x}"),
            },
            0x90 => {}
            0x91..=0x97 => self.r.swap(EAX, (op & 7) as usize),
            0x98 => {
                if size == Size::W {
                    self.set_reg(0, Size::W, self.r[EAX] as u8 as i8 as u16 as u32);
                } else {
                    self.r[EAX] = self.r[EAX] as u16 as i16 as u32;
                }
            }
            0x99 => self.r[EDX] = ((self.r[EAX] as i32) >> 31) as u32,
            0xa8 | 0xa9 => {
                let sz = wide(op);
                let imm = self.fetch(sz);
                self.alu(4, self.reg(0, sz), imm, sz);
            }
            0xa4..=0xa7 | 0xaa..=0xaf => self.string_op(op, wide(op), rep)?,
            0xb0..=0xb7 => {
                let v = self.fetch8() as u32;
                self.set_reg(op & 7, Size::B, v);
            }
            0xb8..=0xbf => {
                let v = self.fetch(size);
                self.set_reg(op & 7, size, v);
            }
            0xc0 | 0xc1 | 0xd0..=0xd3 => {
                let sz = wide(op);
                let (sop, loc) = self.modrm();
                let count = match op {
                    0xc0 | 0xc1 => self.fetch8() as u32,
                    0xd0 | 0xd1 => 1,
                    _ => self.r[ECX] & 0xff,
                };
                let res = self.shift(sop, self.get(loc, sz), count, sz)?;
                self.set(loc, sz, res);
            }
            0xc2 => {
                let n = self.fetch(Size::W);
                self.eip = self.pop();
                self.r[ESP] = self.r[ESP].wrapping_add(n);
            }
            0xc3 => self.eip = self.pop(),
            0xc6 | 0xc7 => {
                let sz = wide(op);
                let (_, loc) = self.modrm();
                let v = self.fetch(sz);
                self.set(loc, sz, v);
            }
            0xc9 => {
                self.r[ESP] = self.r[EBP];
                self.r[EBP] = self.pop();
            }
            0xd6 => self.set_reg(0, Size::B, if self.cf { 0xff } else { 0 }),
            0xd7 => {
                let a = self.r[EBX].wrapping_add(self.r[EAX] & 0xff);
                let v = self.mem.read8(a) as u32;
                self.set_reg(0, Size::B, v);
            }
            0xe0..=0xe2 => {
                let off = self.fetch_s8();
                self.r[ECX] = self.r[ECX].wrapping_sub(1);
                let go = self.r[ECX] != 0
                    && match op {
                        0xe0 => !self.zf,
                        0xe1 => self.zf,
                        _ => true,
                    };
                if go {
                    self.jump(off);
                }
            }
            0xe3 => {
                let off = self.fetch_s8();
                if self.r[ECX] == 0 {
                    self.jump(off);
                }
            }
            0xe8 => {
                let off = self.fetch(Size::D);
                self.push(self.eip);
                self.jump(off);
            }
            0xe9 => {
                let off = self.fetch(Size::D);
                self.jump(off);
            }
            0xeb => {
                let off = self.fetch_s8();
                self.jump(off);
            }
            0xf5 => self.cf = !self.cf,
            0xf6 | 0xf7 => self.group3(wide(op))?,
            0xf8 => self.cf = false,
            0xf9 => self.cf = true,
            0xfc => self.df = false,
            0xfd => self.df = true,
            0xfe | 0xff => {
                let sz = wide(op);
                let (sub, loc) = self.modrm();
                match sub {
                    0 | 1 => {
                        let res = self.inc_dec(self.get(loc, sz), sz, sub == 1);
                        self.set(loc, sz, res);
                    }
                    2 => {
                        let target = self.get(loc, Size::D);
                        self.push(self.eip);
                        self.eip = target;
                    }
                    4 => self.eip = self.get(loc, Size::D),
                    6 => {
                        let v = self.get(loc, Size::D);
                        self.push(v);
                    }
                    _ => bail!("opcode {op:#x}/{sub} at {start:#x}"),
                }
            }
            0x0f => self.two_byte(rep, size, start)?,
            _ => bail!("opcode {op:#x} at {start:#x}"),
        }
        Ok(())
    }

    fn imul(&mut self, reg: u8, a: u32, b: u32, size: Size) {
        let bits = size.bits();
        let sa = ((a << (32 - bits)) as i32 >> (32 - bits)) as i64;
        let sb = ((b << (32 - bits)) as i32 >> (32 - bits)) as i64;
        let wide = sa * sb;
        let res = wide as u32 & size.mask();
        let back = ((res << (32 - bits)) as i32 >> (32 - bits)) as i64;
        self.cf = back != wide;
        self.of = self.cf;
        self.set_reg(reg, size, res);
    }

    fn group3(&mut self, size: Size) -> Result<()> {
        let start = self.eip;
        let (sub, loc) = self.modrm();
        let v = self.get(loc, size);
        match sub {
            0 | 1 => {
                let imm = self.fetch(size);
                self.alu(4, v, imm, size);
            }
            2 => self.set(loc, size, !v),
            3 => {
                let res = self.alu(5, 0, v, size);
                self.set(loc, size, res);
            }
            4 | 5 => match size {
                Size::B => {
                    let al = self.r[EAX] & 0xff;
                    let res = if sub == 4 {
                        al * v
                    } else {
                        ((al as i8 as i32) * (v as i8 as i32)) as u32 & 0xffff
                    };
                    self.set_reg(0, Size::W, res);
                    self.cf = res >> 8 != 0;
                    self.of = self.cf;
                }
                _ => {
                    let wide = if sub == 4 {
                        self.r[EAX] as u64 * v as u64
                    } else {
                        ((self.r[EAX] as i32 as i64) * (v as i32 as i64)) as u64
                    };
                    self.r[EAX] = wide as u32;
                    self.r[EDX] = (wide >> 32) as u32;
                    self.cf = if sub == 4 {
                        self.r[EDX] != 0
                    } else {
                        wide as i64 != wide as u32 as i32 as i64
                    };
                    self.of = self.cf;
                }
            },
            6 | 7 => {
                if v == 0 {
                    bail!("divide by zero near {start:#x}");
                }
                match size {
                    Size::B => {
                        let ax = self.r[EAX] & 0xffff;
                        let (q, r) = if sub == 6 {
                            (ax / v, ax % v)
                        } else {
                            let (a, b) = (ax as i16 as i32, v as i8 as i32);
                            ((a / b) as u32, (a % b) as u32)
                        };
                        if q & !0xff != 0 && sub == 6 {
                            bail!("divide overflow near {start:#x}");
                        }
                        self.set_reg(0, Size::B, q);
                        self.set_reg(4, Size::B, r);
                    }
                    _ => {
                        let num = ((self.r[EDX] as u64) << 32) | self.r[EAX] as u64;
                        let (q, r) = if sub == 6 {
                            (num / v as u64, num % v as u64)
                        } else {
                            let (a, b) = (num as i64, v as i32 as i64);
                            ((a / b) as u64, (a % b) as u64)
                        };
                        if q > u32::MAX as u64 && sub == 6 {
                            bail!("divide overflow near {start:#x}");
                        }
                        self.r[EAX] = q as u32;
                        self.r[EDX] = r as u32;
                    }
                }
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn string_op(&mut self, op: u8, size: Size, rep: u8) -> Result<()> {
        let step = if self.df {
            0u32.wrapping_sub(size.bytes())
        } else {
            size.bytes()
        };
        loop {
            if rep != 0 && self.r[ECX] == 0 {
                break;
            }
            match op {
                0xa4 | 0xa5 => {
                    let v = self.mem.read(self.r[ESI], size);
                    self.mem.write(self.r[EDI], size, v);
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                0xa6 | 0xa7 => {
                    let (a, b) = (
                        self.mem.read(self.r[ESI], size),
                        self.mem.read(self.r[EDI], size),
                    );
                    self.alu(7, a, b, size);
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                0xaa | 0xab => {
                    self.mem.write(self.r[EDI], size, self.reg(0, size));
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
                0xac | 0xad => {
                    let v = self.mem.read(self.r[ESI], size);
                    self.set_reg(0, size, v);
                    self.r[ESI] = self.r[ESI].wrapping_add(step);
                }
                _ => {
                    let b = self.mem.read(self.r[EDI], size);
                    self.alu(7, self.reg(0, size), b, size);
                    self.r[EDI] = self.r[EDI].wrapping_add(step);
                }
            }
            if rep == 0 {
                break;
            }
            self.r[ECX] = self.r[ECX].wrapping_sub(1);
            let compares = matches!(op, 0xa6 | 0xa7 | 0xae | 0xaf);
            if compares && ((rep == 0xf3 && !self.zf) || (rep == 0xf2 && self.zf)) {
                break;
            }
        }
        Ok(())
    }

    fn two_byte(&mut self, rep: u8, size: Size, start: u32) -> Result<()> {
        let op = self.fetch8();
        match op {
            0x80..=0x8f => {
                let off = self.fetch(Size::D);
                if self.cond(op & 15) {
                    self.jump(off);
                }
            }
            0x90..=0x9f => {
                let (_, loc) = self.modrm();
                let v = self.cond(op & 15) as u32;
                self.set(loc, Size::B, v);
            }
            0xa3 | 0xab | 0xb3 | 0xbb => {
                let (reg, loc) = self.modrm();
                let bit = self.reg(reg, size);
                self.bit_op(op, loc, bit, size, true);
            }
            0xba => {
                let (sub, loc) = self.modrm();
                let bit = self.fetch8() as u32;
                let op = [0xa3, 0xab, 0xb3, 0xbb][(sub & 3) as usize];
                if sub < 4 {
                    bail!("opcode 0f ba /{sub} at {start:#x}");
                }
                self.bit_op(op, loc, bit, size, false);
            }
            0xaf => {
                let (reg, loc) = self.modrm();
                let (a, b) = (self.reg(reg, size), self.get(loc, size));
                self.imul(reg, a, b, size);
            }
            0xb6 | 0xb7 | 0xbe | 0xbf => {
                let (reg, loc) = self.modrm();
                let v = match op {
                    0xb6 => self.get(loc, Size::B),
                    0xb7 => self.get(loc, Size::W),
                    0xbe => self.get(loc, Size::B) as u8 as i8 as u32,
                    _ => self.get(loc, Size::W) as u16 as i16 as u32,
                };
                self.set_reg(reg, size, v);
            }
            0x38 if rep == 0xf2 => {
                let op3 = self.fetch8();
                let (reg, loc) = self.modrm();
                let sz = if op3 == 0xf0 { Size::B } else { size };
                let v = self.get(loc, sz);
                let mut crc = self.r[reg as usize];
                for i in 0..sz.bytes() {
                    crc = crc32c_byte(crc, (v >> (8 * i)) as u8);
                }
                self.r[reg as usize] = crc;
            }
            _ => bail!("opcode 0f {op:#x} at {start:#x}"),
        }
        Ok(())
    }

    /// bt/bts/btr/btc. A register bit offset on a memory operand addresses
    /// past the operand, as on the real thing.
    fn bit_op(&mut self, op: u8, loc: Loc, bit: u32, size: Size, reg_offset: bool) {
        let (loc, bit) = match loc {
            Loc::Mem(a) if reg_offset => (
                Loc::Mem(a.wrapping_add(((bit as i32) >> 3) as u32)),
                bit & 7,
            ),
            _ => (loc, bit % size.bits()),
        };
        let sz = if matches!(loc, Loc::Mem(_)) && reg_offset {
            Size::B
        } else {
            size
        };
        let v = self.get(loc, sz);
        self.cf = (v >> bit) & 1 != 0;
        let res = match op {
            0xab => v | (1 << bit),
            0xb3 => v & !(1 << bit),
            0xbb => v ^ (1 << bit),
            _ => return,
        };
        self.set(loc, sz, res);
    }

    /// Run until `eip` reaches `stop`, or fail after `max_steps` instructions.
    pub fn run_until(&mut self, stop: u32, max_steps: u64) -> Result<u64> {
        for n in 0..max_steps {
            if self.eip == stop {
                return Ok(n);
            }
            self.step()?;
        }
        bail!(
            "still running at {:#x} after {max_steps} instructions",
            self.eip
        )
    }
}

fn crc32c_byte(crc: u32, b: u8) -> u32 {
    let mut crc = crc ^ b as u32;
    for _ in 0..8 {
        crc = (crc >> 1) ^ (0x82f6_3b78 & (crc & 1).wrapping_neg());
    }
    crc
}

#[cfg(test)]
#[path = "tests/x86_tests.rs"]
mod tests;
