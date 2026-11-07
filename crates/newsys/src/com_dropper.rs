//! DOS `.com` droppers: Windows intros packed into a `.com` that writes the
//! exe to disk and starts it through int 21h/4B00, which only NTVDM could do.
//! Each is run in [`crate::x86`] with just enough DOS to catch that exe.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use tracing::{info, warn};

use crate::newsys::walk_dir;
use crate::workfile::WorkFile;
use crate::x86::{Cpu, DS, EAX, EBX, ECX, EDX, ES, ESP, Size};

const PSP: u16 = 0x1000;
const MAX_STEPS: u64 = 100_000_000;
const MAX_COM_SIZE: u64 = 0xff00;

struct File {
    name: String,
    data: Vec<u8>,
}

fn file_name(cpu: &Cpu, seg: usize, off: u32) -> String {
    let at = cpu.base(seg) + (off & 0xffff);
    let raw: Vec<u8> = (0..128)
        .map(|i| cpu.mem.read8(at + i))
        .take_while(|&c| c != 0)
        .collect();
    let name = String::from_utf8_lossy(&raw);
    let name = name.rsplit(['\\', ':', '/']).next().unwrap_or_default();
    name.to_ascii_lowercase()
}

/// Run a `.com` until it execs a program it wrote, and return that program.
pub fn run(com: &[u8]) -> Result<Vec<u8>> {
    let mut cpu = Cpu::new();
    cpu.real = true;
    cpu.seg = [PSP; 6];
    let base = cpu.base(DS);
    cpu.mem.load(base, &[0xcd, 0x20]);
    cpu.mem.write(base + 2, Size::W, 0xa000);
    cpu.mem.load(base + 0x80, &[0, 0x0d]);
    cpu.mem.load(base + 0x100, com);
    cpu.r[ESP] = 0xfffe;
    cpu.eip = base + 0x100;

    let mut open: Vec<(u16, File)> = vec![];
    let mut closed: Vec<File> = vec![];
    let mut next_seg = PSP as u32 + 0x1000;
    for _ in 0..MAX_STEPS {
        cpu.step()?;
        let Some(int) = cpu.int.take() else {
            continue;
        };
        let ah = (cpu.r[EAX] >> 8) & 0xff;
        let (bx, cx, dx) = (cpu.r[EBX] & 0xffff, cpu.r[ECX] & 0xffff, cpu.r[EDX]);
        let set_ax = |cpu: &mut Cpu, v: u32| cpu.r[EAX] = (cpu.r[EAX] & !0xffff) | (v & 0xffff);
        cpu.cf = false;
        match (int, ah) {
            (0x21, 0x09 | 0x0e | 0x1a | 0x25 | 0x3b | 0x41 | 0x49 | 0x4a) => {}
            (0x21, 0x19) => set_ax(&mut cpu, 0x1902),
            (0x21, 0x30) => set_ax(&mut cpu, 0x0005),
            (0x21, 0x35) => {
                cpu.seg[ES] = 0;
                cpu.r[EBX] &= !0xffff;
            }
            (0x21, 0x48) => {
                set_ax(&mut cpu, next_seg);
                next_seg += bx;
            }
            (0x21, 0x62) => cpu.r[EBX] = PSP as u32,
            (0x21, 0x3c | 0x5b) => {
                let handle = 5 + open.len() as u16 + closed.len() as u16;
                let name = file_name(&cpu, DS, dx);
                open.push((handle, File { name, data: vec![] }));
                set_ax(&mut cpu, handle as u32);
            }
            (0x21, 0x40) if bx < 5 => set_ax(&mut cpu, cx),
            (0x21, 0x40) => {
                let Some((_, file)) = open.iter_mut().find(|(h, _)| *h as u32 == bx) else {
                    bail!("Write to unknown handle {bx}");
                };
                let at = cpu.base(DS) + (dx & 0xffff);
                file.data.extend(cpu.mem.slice(at, cx));
                set_ax(&mut cpu, cx);
            }
            (0x21, 0x3e) => {
                if let Some(n) = open.iter().position(|(h, _)| *h as u32 == bx) {
                    closed.push(open.remove(n).1);
                }
            }
            (0x21, 0x4b) => {
                let name = file_name(&cpu, DS, dx);
                closed.extend(open.into_iter().map(|(_, f)| f));
                let Some(file) = closed.into_iter().rev().find(|f| f.name == name) else {
                    bail!("Starts {name:?}, which it did not write");
                };
                return Ok(file.data);
            }
            (0x20, _) | (0x21, 0x00 | 0x4c) => bail!("Exited without starting a program"),
            _ => bail!("Unhandled int {int:#x} ah={ah:#x}"),
        }
    }
    bail!("Still running after {MAX_STEPS} instructions")
}

fn files_with_ext(dir: &Path, wanted: &str) -> Result<Vec<PathBuf>> {
    let mut found = vec![];
    walk_dir(dir, 0, |path, ext, _| {
        if ext == wanted {
            found.push(path.to_owned());
        }
        Ok(())
    })?;
    Ok(found)
}

fn droppers(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut found = files_with_ext(dir, "com")?;
    found.retain(|p| fs::metadata(p).is_ok_and(|m| m.len() <= MAX_COM_SIZE));
    Ok(found)
}

/// Replace each `.com` in a Windows release with the exe it drops, on a copy.
/// A release that ships exes of its own keeps those, since a dropped exe can
/// be a tiny PE that wine will not load.
pub fn unpack_release(file: &mut WorkFile) -> Result<()> {
    if file.is_in_place()
        || file.get_meta_or("platform", "") != "Windows"
        || droppers(&file.path)?.is_empty()
    {
        return Ok(());
    }
    file.make_temp()?;
    let has_exes = !files_with_ext(&file.path, "exe")?.is_empty();
    for com in droppers(&file.path)? {
        match run(&fs::read(&com)?) {
            Ok(data) if data.starts_with(b"MZ") => {
                info!("Unpacked COM dropper {com:?}");
                if !has_exes {
                    fs::write(com.with_extension("exe"), data)?;
                }
                fs::remove_file(&com)?;
            }
            Ok(_) => warn!("COM dropper {com:?} starts something that is not an exe"),
            Err(e) => warn!("Can't unpack COM dropper {com:?}: {e:#}"),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/com_dropper_tests.rs"]
mod tests;
