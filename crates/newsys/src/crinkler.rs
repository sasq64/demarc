//! Crinkler-packed executables, unpacked into ordinary PE files.
//!
//! A Crinkler exe has no import table: its unpacked code finds each function
//! by hashing the export names of the DLLs it loads, and wine's export tables
//! are not Windows' (docs/CRINKLER.md). Here the depacker stub is run in
//! [`crate::x86`], the hashes are matched against the export lists Crinkler
//! itself was built with, and the result is written out as a PE with a real
//! import table, which wine's loader resolves by name. The unpacked code also
//! takes an override `patch` at a fixed offset, which the packed file cannot.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Context, Result, bail};
use tracing::{debug, info, warn};

use crate::cache::{FileCache, KeyHasher};
use crate::newsys::walk_dir;
use crate::system_dir;
use crate::wine::is_yes;
use crate::workfile::WorkFile;
use crate::x86::{Cpu, EAX, EBX, ESP};

const IMAGE_BASE: u32 = 0x40_0000;
/// Where every Crinkler header unpacks to (`CRINKLER_CODEBASE`).
const CODE_BASE: u32 = 0x42_0000;
const PEB: u32 = 0x7ffd_f000;
// Below the image, so nothing the stack holds is taken for unpacked data.
const STACK_TOP: u32 = 0x0030_0000;
const MAX_STEPS: u64 = 20_000_000_000;

/// The Crinkler version that packed `data` (`22` for 2.2), if it is one.
pub fn version(data: &[u8]) -> Option<u32> {
    if data.len() < 0x100 || &data[0..2] != b"MZ" {
        return None;
    }
    let digits = match u32::from_le_bytes(data[0x3c..0x40].try_into().ok()?) {
        4 if &data[4..8] == b"PE\0\0" => &data[2..4],
        12 if &data[12..16] == b"PE\0\0" => &data[38..40],
        _ => return None,
    };
    if !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let v = (digits[0] - b'0') as u32 * 10 + (digits[1] - b'0') as u32;
    // 0.6 was renamed 1.0
    Some(if v == 6 { 10 } else { v })
}

fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// Run the depacker and return the unpacked code from [`CODE_BASE`] on.
pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let pe = u32_at(data, 0x3c) as usize;
    let entry = IMAGE_BASE + u32_at(data, pe + 0x28);
    let mut cpu = Cpu::new();
    cpu.mem.load(IMAGE_BASE, data);
    // Pre-1.4 headers also map the start of the file a second time, as a
    // section; the loader rounds its file offset down to 512.
    let sections = u16::from_le_bytes([data[pe + 6], data[pe + 7]]) as usize;
    let table = pe + 24 + u16::from_le_bytes([data[pe + 20], data[pe + 21]]) as usize;
    for i in 0..sections {
        let at = table + 40 * i;
        let (rva, raw, ptr) = (
            u32_at(data, at + 12),
            u32_at(data, at + 16) as usize,
            u32_at(data, at + 20) as usize & !0x1ff,
        );
        if let Some(bytes) = data.get(ptr..(ptr + raw).min(data.len())) {
            cpu.mem.load(IMAGE_BASE + rva, bytes);
        }
    }
    cpu.r[EAX] = entry;
    cpu.r[EBX] = PEB;
    cpu.r[ESP] = STACK_TOP;
    cpu.push(0);
    cpu.eip = entry;
    let steps = cpu
        .run_until(CODE_BASE, MAX_STEPS)
        .context("Crinkler depacker did not reach the unpacked code")?;
    debug!("Crinkler depacker ran {steps} instructions");
    let end = cpu
        .mem
        .last_nonzero(CODE_BASE)
        .context("Crinkler depacker wrote nothing")?;
    Ok(cpu.mem.slice(CODE_BASE, end + 1 - CODE_BASE))
}

/// Export names per DLL, as the Crinkler version that packed an exe knew them
/// (`system/win/crinkler/exports.txt`, from `tools/crinkler/gen_exports.py`).
pub struct Exports {
    dlls: HashMap<String, Vec<String>>,
    /// Names in Windows' ordinal order, where that is not alphabetical.
    ordinals: HashMap<String, Vec<String>>,
}

impl Exports {
    /// `text` lists names under `[dll]`, each optionally followed by the
    /// versions that have it (`23-`, `20-22`, `20-20,23-`).
    pub fn parse(text: &str, version: u32) -> Self {
        let has = |ranges: &str| {
            ranges.split(',').any(|r| {
                let (lo, hi) = r.split_once('-').unwrap_or((r, r));
                let lo = lo.parse().unwrap_or(0);
                let hi = hi.parse().unwrap_or(u32::MAX);
                (lo..=hi).contains(&version.max(20))
            })
        };
        let mut dlls: HashMap<String, Vec<String>> = HashMap::new();
        let mut ordinals: HashMap<String, Vec<String>> = HashMap::new();
        let mut current = None;
        for line in text
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            if let Some(dll) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = Some(dll.to_owned());
                continue;
            }
            let (name, ranges) = line.split_once(' ').unwrap_or((line, ""));
            let Some(dll) = &current else { continue };
            if let Some(dll) = dll.strip_suffix(" ordinals") {
                ordinals
                    .entry(dll.to_owned())
                    .or_default()
                    .push(name.to_owned());
            } else if ranges.is_empty() || has(ranges) {
                dlls.entry(dll.clone()).or_default().push(name.to_owned());
            }
        }
        for names in dlls.values_mut() {
            names.sort();
        }
        Self { dlls, ordinals }
    }

    pub fn load(path: &Path, version: u32) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("Can't read {path:?}"))?;
        Ok(Self::parse(&text, version))
    }

    fn names(&self, dll: &str) -> Result<&[String]> {
        self.dlls
            .get(dll)
            .map(Vec::as_slice)
            .with_context(|| format!("No export list for {dll}"))
    }

    /// `name` and the `count - 1` exports after it in ordinal order.
    fn range(&self, dll: &str, name: &str, count: usize) -> Result<Vec<String>> {
        let order = match self.ordinals.get(dll) {
            Some(order) => order.as_slice(),
            None => self.names(dll)?,
        };
        let at = order
            .iter()
            .position(|n| n == name)
            .with_context(|| format!("{name} has no ordinal in {dll}"))?;
        order
            .get(at..at + count)
            .map(<[String]>::to_vec)
            .with_context(|| format!("range of {count} from {name} runs past {dll}'s exports"))
    }
}

/// One function the unpacked code calls through a pointer at `slot`.
#[derive(Debug, PartialEq)]
pub struct Import {
    pub slot: u32,
    pub dll: String,
    pub name: String,
}

/// What the import code at the start of the unpacked data does, and where.
#[derive(Debug)]
pub struct ImportCode {
    /// Offsets into the unpacked code: the import loop, replaced by a jump.
    pub start: usize,
    pub end: usize,
    pub imports: Vec<Import>,
}

fn find(code: &[u8], pattern: &[Option<u8>]) -> Option<usize> {
    code.windows(pattern.len()).position(|w| {
        w.iter()
            .zip(pattern)
            .all(|(b, p)| p.is_none_or(|p| p == *b))
    })
}

/// A byte pattern with `None` for "any".
macro_rules! pat {
    ($($b:tt),*) => { [$(pat!(@ $b)),*] };
    (@ _) => { None };
    (@ $b:expr) => { Some($b) };
}

fn cstr_at(code: &[u8], at: usize) -> Option<&[u8]> {
    let len = code.get(at..)?.iter().position(|b| *b == 0)?;
    Some(&code[at..at + len])
}

fn dll_name(raw: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(raw).ok()?.to_ascii_lowercase();
    let name = name.strip_suffix(".dll").unwrap_or(&name).to_owned();
    (!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
        .then_some(name)
}

/// The hash Crinkler's tiny import computes for an export name.
fn tiny_hash(name: &str, mult: u32, shift: u32) -> u32 {
    let mut eax = 0u32;
    for c in name.bytes().chain(std::iter::once(0)) {
        eax = ((eax & !0xff) | c as u32).wrapping_mul(mult);
        let al = (eax << 1) & 0xff;
        eax = (eax & !0xff) | al;
        if al == 0 {
            break;
        }
    }
    eax >> shift
}

/// The hash Crinkler's ordinary import computes for an export name.
fn hash(name: &str) -> u32 {
    name.bytes()
        .chain(std::iter::once(0))
        .fold(0u32, |h, c| h.rotate_left(6) ^ c as u32)
}

/// Find and decode the import code of an unpacked image. `header` is the
/// packed file, which holds the hash list of the ordinary import.
pub fn find_imports(code: &[u8], header: &[u8], exports: &Exports) -> Result<ImportCode> {
    let at = |addr: u32| (addr - CODE_BASE) as usize;
    // 1.x reads the PEB itself (xor eax, eax; mov eax, fs:[eax+30h], or
    // mov eax, fs:[bx+30h] from 1.2), later
    // versions pop it from where the depacker pushed it.
    let ordinary = find(
        code,
        &pat!(
            0xbb, _, _, _, _, 0xbe, _, _, _, _, 0xbf, _, _, _, _, 0x58, 0x8b, 0x40, 0x0c
        ),
    )
    .or_else(|| {
        find(
            code,
            &pat!(
                0xbb, _, _, _, _, 0xbe, _, _, _, _, 0xbf, _, _, _, _, 0x31, 0xc0, 0x64, 0x8b, 0x40,
                0x30
            ),
        )
    })
    .or_else(|| {
        find(
            code,
            &pat!(0xbb, _, _, _, _, 0xbe, _, _, _, _, 0xbf, _, _, _, _, 0x64, 0x67, 0x8b, 0x47, 0x30),
        )
    });
    // Tiny import: every export is hashed into a table of pointers.
    let tiny = find(
        code,
        &pat!(0xbf, _, _, _, _, 0x58, 0x8b, 0x40, 0x0c, 0x8b, 0x40, 0x0c),
    );
    if let Some(start) = tiny.filter(|_| ordinary.is_none()) {
        let names = u32_at(code, start + 1);
        let h = find(
            code,
            &pat!(
                0x69, 0xc0, _, _, _, _, 0x00, 0xc0, 0x75, _, 0xc1, 0xe8, _, 0x89, 0x14, 0x85
            ),
        )
        .context("tiny import without its hash loop")?;
        let (mult, shift, table) = (
            u32_at(code, h + 2),
            code[h + 12] as u32,
            u32_at(code, h + 16),
        );
        let e = find(&code[start..], &pat!(0x83, 0xc7, _, 0x85, 0xc0, 0x75, _))
            .context("tiny import without its end")?;
        let stride = code[start + e + 2] as usize;
        let end = start + e + 7;

        let mut dlls = vec!["kernel32".to_owned()];
        let mut p = at(names);
        // The list has no end marker: the loop stops when LoadLibrary fails.
        while let Some(name) = cstr_at(code, p)
            .and_then(dll_name)
            .filter(|n| exports.dlls.contains_key(n))
        {
            dlls.push(name);
            p += stride;
        }
        // The last DLL to write a slot owns it, and within one DLL the
        // alphabetically last name (crinkler's FindCollisionFreeHash).
        let mut owner: HashMap<u32, (usize, &str)> = HashMap::new();
        for (order, dll) in dlls.iter().enumerate() {
            for name in exports.names(dll)? {
                let slot = tiny_hash(name, mult, shift);
                let new = (order, name.as_str());
                if owner.get(&slot).is_none_or(|old| *old < new) {
                    owner.insert(slot, new);
                }
            }
        }
        let slots = 1u32 << (32 - shift);
        let mut used: Vec<u32> = (0..code.len().saturating_sub(3))
            .filter(|i| !(start..end).contains(i))
            .map(|i| u32_at(code, i))
            .filter(|v| (table..table + 4 * slots).contains(v) && (v - table) % 4 == 0)
            .collect();
        used.sort_unstable();
        used.dedup();
        let mut imports = vec![];
        for addr in used {
            match owner.get(&((addr - table) / 4)) {
                Some((order, name)) => imports.push(Import {
                    slot: addr,
                    dll: dlls[*order].clone(),
                    name: (*name).to_owned(),
                }),
                // A DLL without an export list, or a lookalike constant.
                None if references(code, addr, start..end) => {
                    bail!("nothing known hashes to the slot at {addr:#x} ({dlls:?})")
                }
                None => {}
            }
        }
        return Ok(ImportCode {
            start,
            end,
            imports,
        });
    }

    // Ordinary import: a list of 32-bit hashes, one pointer each.
    let start = ordinary.context("no Crinkler import code found")?;
    let (hashes, names, iat) = (
        u32_at(code, start + 1),
        u32_at(code, start + 6),
        u32_at(code, start + 11),
    );
    // Range import: each hash also takes the exports after it, a count byte each.
    let range = find(
        &code[start..],
        &pat!(0x8b, 0x02, 0x01, 0xe8, 0x83, 0xc2, 0x04, 0xab),
    )
    .is_some();
    let dll_loop = start + 0x1d;
    if code.get(dll_loop..dll_loop + 3) == Some(&[0x31, 0xc0, 0xac])
        && code.get(dll_loop + 3) != Some(&0x91)
    {
        bail!("fallback DLL lists are not handled");
    }
    let e =
        find(&code[start..], &pat!(0xfe, 0xc0, 0x74, _)).context("import code without its end")?;
    let end = start + e + 4;
    let hash_at = |i: usize| -> Result<u32> {
        let a = (hashes - IMAGE_BASE) as usize + 4 * i;
        header
            .get(a..a + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .context("hash list outside the header")
    };
    let mut imports = vec![];
    let mut hashes_read = 0;
    let mut slots = 0;
    let mut dll = "kernel32".to_owned();
    let mut p = at(names);
    loop {
        let count = *code.get(p).context("DLL list runs off the code")? as usize;
        p += 1;
        let list = exports.names(&dll)?;
        for _ in 0..count {
            let h = hash_at(hashes_read)?;
            hashes_read += 1;
            let n = if range {
                p += 1;
                match code[p - 1] {
                    0 => 256,
                    n => n as usize,
                }
            } else {
                1
            };
            let first = iat + 4 * slots as u32;
            slots += n;
            let Some(name) = list.iter().find(|n| hash(n) == h) else {
                // 1.x headers overlap the hash list, so some are dummies.
                if (first..first + 4 * n as u32).any(|slot| references(code, slot, start..end)) {
                    bail!("no export of {dll} hashes to {h:#x}");
                }
                continue;
            };
            let names = if range {
                exports.range(&dll, name, n)?
            } else {
                vec![name.clone()]
            };
            for (k, name) in names.into_iter().enumerate() {
                imports.push(Import {
                    slot: first + 4 * k as u32,
                    dll: dll.clone(),
                    name,
                });
            }
        }
        if code.get(p) == Some(&0xff) {
            break;
        }
        let raw = cstr_at(code, p).context("DLL list runs off the code")?;
        p += raw.len() + 1;
        dll = dll_name(raw).with_context(|| format!("bad DLL name {raw:?}"))?;
    }
    Ok(ImportCode {
        start,
        end,
        imports,
    })
}

/// Whether `code` outside `skip` reads a pointer from `addr`: `call`/`jmp
/// [addr]`, `mov eax, [addr]` or `mov reg, [addr]`.
fn references(code: &[u8], addr: u32, skip: std::ops::Range<usize>) -> bool {
    code.windows(6).enumerate().any(|(i, w)| {
        let operand = |at: usize| w[at..at + 4] == addr.to_le_bytes();
        !skip.contains(&i)
            && ((w[0] == 0xff && matches!(w[1], 0x15 | 0x25) && operand(2))
                || (w[0] == 0x8b && w[1] & 0xc7 == 0x05 && operand(2))
                || (w[1] == 0xa1 && operand(2)))
    })
}

fn align(v: u32, a: u32) -> u32 {
    (v + a - 1) & !(a - 1)
}

/// The packed exe `data` as an ordinary PE: the unpacked code at
/// [`CODE_BASE`], and an import table whose entries a stub copies into the
/// slots the code calls through, in place of the import loop.
pub fn rebuild(data: &[u8], exports: &Exports) -> Result<Vec<u8>> {
    let mut code = decompress(data)?;
    let mut found = find_imports(&code, data, exports)?;
    // Pixel formats and swaps set through gdi32 never reach the opengl32
    // proxy (tools/opengl32); gdi32 hands them to opengl32 on Windows too.
    if found.imports.iter().any(|i| i.dll == "opengl32") {
        for imp in &mut found.imports {
            let wgl = matches!(
                imp.name.as_str(),
                "ChoosePixelFormat"
                    | "DescribePixelFormat"
                    | "GetPixelFormat"
                    | "SetPixelFormat"
                    | "SwapBuffers"
            );
            if imp.dll == "gdi32" && wgl {
                imp.dll = "opengl32".into();
                imp.name = format!("wgl{}", imp.name);
            }
        }
    }
    for imp in &found.imports {
        debug!("Crinkler import {:#x}: {}!{}", imp.slot, imp.dll, imp.name);
    }
    let pe = u32_at(data, 0x3c) as usize;
    let size_of_image = u32_at(data, pe + 0x50);
    let subsystem = u16::from_le_bytes([data[pe + 0x5c], data[pe + 0x5d]]);
    let large_address = u16::from_le_bytes([data[pe + 0x16], data[pe + 0x17]]) & 0x20;
    // Stack and heap sizes; Crinkler's give the main thread tens of megabytes.
    let sizes = |at: usize| u32_at(data, pe + 0x60 + at);

    const IDATA: u32 = 0x1000;
    let mut sec: Vec<u8> = vec![];
    let here = |sec: &Vec<u8>| IMAGE_BASE + IDATA + sec.len() as u32;
    let rel = |from: u32, to: u32| to.wrapping_sub(from).to_le_bytes();

    // Entry: the PEB, and the unpacked code's address when the call transform
    // pops it, as the depacker leaves them.
    let entry = here(&sec);
    sec.extend([0x64, 0xa1, 0x30, 0, 0, 0, 0x50]);
    if code.starts_with(&[0x5f, 0xb9]) {
        sec.push(0x68);
        sec.extend(CODE_BASE.to_le_bytes());
    }
    sec.push(0xe9);
    sec.extend(rel(here(&sec) + 4, CODE_BASE));

    // The import loop's replacement: drop the PEB, fill the slots.
    let fill = here(&sec);
    sec.push(0x58);
    let fill_at = sec.len();
    sec.resize(fill_at + 10 * found.imports.len(), 0);
    sec.extend([0x31, 0xc0, 0xe9]);
    sec.extend(rel(here(&sec) + 4, CODE_BASE + found.end as u32));
    sec.resize(align(sec.len() as u32, 16) as usize, 0);

    // A maximized window created at 0x0 never reaches gamescope's session
    // (Organix); Windows gives it the screen's size, so the wrapper does.
    let wrap_window = found
        .imports
        .iter()
        .any(|i| i.dll == "user32" && i.name == "CreateWindowExA");
    let mut table: Vec<(&str, &str)> = found
        .imports
        .iter()
        .map(|i| (i.dll.as_str(), i.name.as_str()))
        .collect();
    if wrap_window {
        table.push(("user32", "GetSystemMetrics"));
    }
    table.dedup();

    let mut groups: Vec<(&str, Vec<&str>)> = vec![];
    for (dll, name) in &table {
        match groups.iter_mut().find(|(d, _)| d == dll) {
            Some((_, list)) if !list.contains(name) => list.push(name),
            Some(_) => {}
            None => groups.push((dll, vec![name])),
        }
    }
    let desc_at = sec.len();
    sec.resize(desc_at + 20 * (groups.len() + 1), 0);
    let mut iat_of: HashMap<(&str, &str), u32> = HashMap::new();
    for (gi, (dll, list)) in groups.iter().enumerate() {
        let name_rva = IDATA + sec.len() as u32;
        sec.extend(format!("{dll}.dll\0").bytes());
        sec.resize(align(sec.len() as u32, 4) as usize, 0);
        let mut hints = vec![];
        for name in list {
            hints.push(IDATA + sec.len() as u32);
            sec.extend([0, 0]);
            sec.extend(name.bytes());
            sec.push(0);
            sec.resize(align(sec.len() as u32, 2) as usize, 0);
        }
        sec.resize(align(sec.len() as u32, 4) as usize, 0);
        let ilt = IDATA + sec.len() as u32;
        hints
            .iter()
            .chain(&[0])
            .for_each(|h| sec.extend(h.to_le_bytes()));
        let iat = IDATA + sec.len() as u32;
        for (k, name) in list.iter().enumerate() {
            iat_of.insert((dll, name), IMAGE_BASE + iat + 4 * k as u32);
        }
        hints
            .iter()
            .chain(&[0])
            .for_each(|h| sec.extend(h.to_le_bytes()));
        let d = desc_at + 20 * gi;
        for (i, v) in [ilt, 0, 0, name_rva, iat].iter().enumerate() {
            sec[d + 4 * i..d + 4 * i + 4].copy_from_slice(&v.to_le_bytes());
        }
    }

    let window_wrapper = wrap_window.then(|| {
        let at = here(&sec);
        let metrics = iat_of[&("user32", "GetSystemMetrics")].to_le_bytes();
        let create = iat_of[&("user32", "CreateWindowExA")].to_le_bytes();
        // test byte [esp+13h], 1 (WS_MAXIMIZE); jz go; cmp dword [esp+1ch], 0 (width); jne go
        sec.extend([
            0xf6, 0x44, 0x24, 0x13, 0x01, 0x74, 0x1f, 0x83, 0x7c, 0x24, 0x1c, 0x00, 0x75, 0x18,
        ]);
        for (index, arg) in [(0, 0x1c), (1, 0x20)] {
            // push SM_CXSCREEN/SM_CYSCREEN; call [GetSystemMetrics]; mov [esp+arg], eax
            sec.extend([0x6a, index, 0xff, 0x15]);
            sec.extend(metrics);
            sec.extend([0x89, 0x44, 0x24, arg]);
        }
        // go: jmp [CreateWindowExA]
        sec.extend([0xff, 0x25]);
        sec.extend(create);
        at
    });

    for (k, imp) in found.imports.iter().enumerate() {
        let at = fill_at + 10 * k;
        let wrapped =
            window_wrapper.filter(|_| imp.dll == "user32" && imp.name == "CreateWindowExA");
        if let Some(wrapper) = wrapped {
            // mov dword [slot], wrapper
            sec[at..at + 2].copy_from_slice(&[0xc7, 0x05]);
            sec[at + 2..at + 6].copy_from_slice(&imp.slot.to_le_bytes());
            sec[at + 6..at + 10].copy_from_slice(&wrapper.to_le_bytes());
        } else {
            sec[at] = 0xa1;
            sec[at + 1..at + 5]
                .copy_from_slice(&iat_of[&(imp.dll.as_str(), imp.name.as_str())].to_le_bytes());
            sec[at + 5] = 0xa3;
            sec[at + 6..at + 10].copy_from_slice(&imp.slot.to_le_bytes());
        }
    }
    code[found.start] = 0xe9;
    code[found.start + 1..found.start + 5]
        .copy_from_slice(&rel(CODE_BASE + found.start as u32 + 5, fill));

    const FILE_ALIGN: u32 = 0x200;
    const SECTION_ALIGN: u32 = 0x1000;
    const HEADERS: u32 = 0x400;
    let code_rva = CODE_BASE - IMAGE_BASE;
    let code_vsize = size_of_image
        .saturating_sub(code_rva)
        .max(code.len() as u32);
    let idata_raw = align(sec.len() as u32, FILE_ALIGN);
    let code_raw = align(code.len() as u32, FILE_ALIGN);

    let mut h = vec![0u8; HEADERS as usize];
    let put =
        |h: &mut Vec<u8>, at: usize, bytes: &[u8]| h[at..at + bytes.len()].copy_from_slice(bytes);
    put(&mut h, 0, b"MZ");
    put(&mut h, 0x3c, &0x40u32.to_le_bytes());
    put(&mut h, 0x40, b"PE\0\0");
    let coff = 0x44;
    put(&mut h, coff, &0x14cu16.to_le_bytes());
    put(&mut h, coff + 2, &2u16.to_le_bytes());
    put(&mut h, coff + 16, &0xe0u16.to_le_bytes());
    put(&mut h, coff + 18, &(0x0103 | large_address).to_le_bytes());
    let opt = coff + 20;
    let fields: [(usize, u32, usize); 22] = [
        (0, 0x10b, 2),
        (4, code_raw, 4),
        (8, idata_raw, 4),
        (16, entry - IMAGE_BASE, 4),
        (20, IDATA, 4),
        (24, code_rva, 4),
        (28, IMAGE_BASE, 4),
        (32, SECTION_ALIGN, 4),
        (36, FILE_ALIGN, 4),
        (40, 4, 2),
        (48, 4, 2),
        (56, align(code_rva + code_vsize, SECTION_ALIGN), 4),
        (60, HEADERS, 4),
        (68, subsystem as u32, 2),
        (72, sizes(0), 4),
        (76, sizes(4), 4),
        (80, sizes(8), 4),
        (84, sizes(12), 4),
        (92, 16, 4),
        (104, IDATA + desc_at as u32, 4),
        (108, 20 * (groups.len() as u32 + 1), 4),
        (2, 14, 1),
    ];
    for (at, v, n) in fields {
        put(&mut h, opt + at, &v.to_le_bytes()[..n]);
    }
    let sections = [
        (
            b".crimp\0\0",
            align(sec.len() as u32, SECTION_ALIGN),
            IDATA,
            idata_raw,
            HEADERS,
        ),
        (
            b".crink\0\0",
            code_vsize,
            code_rva,
            code_raw,
            HEADERS + idata_raw,
        ),
    ];
    for (i, (name, vsize, rva, raw, ptr)) in sections.iter().enumerate() {
        let s = opt + 0xe0 + 40 * i;
        put(&mut h, s, *name);
        for (k, v) in [*vsize, *rva, *raw, *ptr].iter().enumerate() {
            put(&mut h, s + 8 + 4 * k, &v.to_le_bytes());
        }
        put(&mut h, s + 36, &0xe000_0060u32.to_le_bytes());
    }

    let mut out = h;
    out.extend(&sec);
    out.resize((HEADERS + idata_raw) as usize, 0);
    out.extend(&code);
    out.resize((HEADERS + idata_raw + code_raw) as usize, 0);
    Ok(out)
}

/// Meta that, set to `false`, leaves a release's Crinkler exes packed.
pub const UNPACK_META: &str = "crinkler_unpack";

static CACHE: LazyLock<FileCache> = LazyLock::new(|| FileCache::new("crinkler", 64 << 20));

/// The largest file looked at; Crinkler's output is a few kilobytes.
const MAX_SIZE: u64 = 1 << 20;

/// Part of the cache key: bump it when [`rebuild`] writes something else.
const REBUILD_FORMAT: &str = "4";

fn crinkler_exes(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = vec![];
    walk_dir(dir, 2, |path, ext, header| {
        if ext == "exe"
            && header == b"MZ"
            && fs::metadata(path)?.len() <= MAX_SIZE
            && version(&fs::read(path)?).is_some()
        {
            found.push(path.to_owned());
        }
        Ok(())
    })?;
    Ok(found)
}

/// Replace every Crinkler exe in the release with its [rebuilt](rebuild)
/// form, on a copy. One that cannot be rebuilt stays as it is.
pub fn unpack_release(file: &mut WorkFile) -> Result<()> {
    if file.is_in_place() || !is_yes(&file.get_meta_or(UNPACK_META, "true")) {
        return Ok(());
    }
    if crinkler_exes(&file.path)?.is_empty() {
        return Ok(());
    }
    file.make_temp()?;
    let list = system_dir().join("win/crinkler/exports.txt");
    let list_text = fs::read_to_string(&list).with_context(|| format!("Can't read {list:?}"))?;
    // A release may ship one exe per resolution; each takes seconds.
    std::thread::scope(|scope| {
        let jobs: Vec<_> = crinkler_exes(&file.path)?
            .into_iter()
            .map(|exe| scope.spawn(|| unpack_exe(exe, &list_text)))
            .collect();
        for job in jobs {
            job.join()
                .map_err(|_| anyhow::anyhow!("Crinkler unpacking panicked"))??;
        }
        Ok(())
    })
}

fn unpack_exe(exe: PathBuf, list_text: &str) -> Result<()> {
    let data = fs::read(&exe)?;
    let version = version(&data).unwrap_or(0);
    let mut key = KeyHasher::new();
    key.add(REBUILD_FORMAT);
    key.add(&data);
    key.add(list_text);
    let rebuilt = CACHE.get_file(&key.finish(), "rebuilt.exe", |out| {
        let exports = Exports::parse(list_text, version);
        Ok(fs::write(out, rebuild(&data, &exports)?)?)
    });
    match rebuilt {
        Ok(path) => {
            info!(
                "Unpacked Crinkler {}.{} exe {exe:?}",
                version / 10,
                version % 10
            );
            fs::copy(path, &exe)?;
        }
        Err(e) => warn!("Leaving Crinkler exe {exe:?} packed: {e:#}"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/crinkler_tests.rs"]
mod tests;
