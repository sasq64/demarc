#!/usr/bin/env python3
"""Write the 128-byte CMOS image an AT-class PCem machine needs to boot.

A 486 BIOS keeps its whole idea of the machine -- how many floppy drives, what
geometry the hard disc has, whether there is a VGA -- in battery-backed CMOS,
and with none of that filled in it stops at "RUN SETUP UTILITY / Press <F1> to
RESUME" and never looks at drive C. PCem faithfully starts every new machine
with an empty CMOS, so a config that is supposed to boot unattended has to
bring its own.

The layout is the standard AT one that every AMI/Award BIOS of the era shares:
offsets 0x10..0x2D are the settings, 0x2E/0x2F their checksum, and the BIOS
refuses everything if that sum does not match.

    scripts/make-pc-cmos.py testdata/pc/ami486.nvr --mem 8192 \\
        --hdd-cylinders 120 --hdd-heads 4 --hdd-sectors 17

PCem reads it as <nvr>/default/<romset>.nvr, or -- in the libretro build -- as
<name>.nvr next to the .cfg, which is where testdata keeps it. It is only ever
read from there; the machine's own writes go to the save directory.
"""

import argparse
from pathlib import Path


def bcd(n: int) -> int:
    return (n // 10) << 4 | (n % 10)


# Floppy drive types as the BIOS numbers them, high nibble A: and low nibble B:.
FLOPPY_TYPES = {"none": 0, "360": 1, "1.2": 2, "720": 3, "1.44": 4, "2.88": 6}


def build(mem_kb: int, floppy_a: str, cylinders: int, heads: int, sectors: int) -> bytes:
    cmos = bytearray(128)

    # Clock. The date is arbitrary but has to be legal, or the BIOS reports an
    # invalid time and drops into setup for that reason instead.
    cmos[0x00] = bcd(0)  # seconds
    cmos[0x02] = bcd(0)  # minutes
    cmos[0x04] = bcd(12)  # hours
    cmos[0x06] = bcd(5)  # day of week
    cmos[0x07] = bcd(7)  # day of month
    cmos[0x08] = bcd(10)  # month
    cmos[0x09] = bcd(93)  # year
    cmos[0x0A] = 0x26  # status A: 32kHz, 1024Hz periodic
    cmos[0x0B] = 0x02  # status B: 24-hour, BCD
    cmos[0x0D] = 0x80  # status D: battery good

    cmos[0x0E] = 0x00  # diagnostic: nothing wrong
    cmos[0x0F] = 0x00  # shutdown status: normal power-on

    cmos[0x10] = FLOPPY_TYPES[floppy_a] << 4  # A: as given, no B:
    cmos[0x12] = 0xF0  # drive C: uses the extended type below, no drive D:

    # Equipment: one floppy drive, coprocessor present, EGA/VGA display
    # (bits 4-5 zero). Getting the display bits wrong is what produces the
    # "CMOS display type mismatch" complaint.
    cmos[0x14] = 0x07

    base_kb = 640
    cmos[0x15] = base_kb & 0xFF
    cmos[0x16] = base_kb >> 8
    ext_kb = max(0, mem_kb - 1024)
    cmos[0x17] = ext_kb & 0xFF
    cmos[0x18] = (ext_kb >> 8) & 0xFF

    # Type 47 means "user-defined", i.e. read the geometry from 0x1B..0x23
    # rather than from the BIOS's built-in table of drive types.
    cmos[0x19] = 47
    cmos[0x1B] = cylinders & 0xFF
    cmos[0x1C] = cylinders >> 8
    cmos[0x1D] = heads
    cmos[0x1E] = 0xFF  # write precompensation: none
    cmos[0x1F] = 0xFF
    cmos[0x20] = 0x00 if heads <= 8 else 0x08  # control byte
    cmos[0x21] = cylinders & 0xFF  # landing zone
    cmos[0x22] = cylinders >> 8
    cmos[0x23] = sectors

    checksum = sum(cmos[0x10:0x2E]) & 0xFFFF
    cmos[0x2E] = checksum >> 8
    cmos[0x2F] = checksum & 0xFF

    # The POST re-reads the extended memory size from here and compares.
    cmos[0x30] = ext_kb & 0xFF
    cmos[0x31] = (ext_kb >> 8) & 0xFF
    cmos[0x32] = 0x19  # century, BCD

    return bytes(cmos)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("out", type=Path)
    ap.add_argument("--mem", type=int, default=8192, help="total RAM in KB (must match mem_size in the .cfg)")
    ap.add_argument("--floppy-a", choices=sorted(FLOPPY_TYPES), default="1.44")
    ap.add_argument("--hdd-cylinders", type=int, default=0)
    ap.add_argument("--hdd-heads", type=int, default=0)
    ap.add_argument("--hdd-sectors", type=int, default=0)
    args = ap.parse_args()

    args.out.write_bytes(
        build(args.mem, args.floppy_a, args.hdd_cylinders, args.hdd_heads, args.hdd_sectors)
    )
    print(f"wrote {args.out}")


if __name__ == "__main__":
    main()
