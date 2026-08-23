#!/usr/bin/env python3
"""Patch a Retro Replay 3.8 cartridge image to boot straight to BASIC.

The stock ROM draws a boot menu and waits for a function key:

    F1 - configure memory
    F3 - normal reset
    F5 - utilities
    F7 - install fastload      <- the one we want

This patch makes the ROM take the F7 branch by itself, and hides the menu
while doing so. The menu-drawing routine is not skipped, because it also
probes the SilverSurfer/clockport hardware and pokes $de01/$de0f; instead the
KERNAL screen page ($0288) is pointed at $c000 for the duration of the call,
so the routine runs unchanged and its output lands off-screen. The screen is
cleared afterwards, then the F7 branch is taken with no key wait.

All patches are inside bank 0 of the cartridge.

Usage: rr_autoboot.py <in.crt> [out.crt]
"""
import struct
import sys

# --- bank 0 addresses (Retro Replay 3.8p) ---------------------------------
FREE = 0x8102       # ROM filler between the header block and the cold start
DRAW_CALL = 0x8244  # JSR $9f03 / .word $8054 -- far call to bank 3, draws menu
KEY_READ = 0x8251   # JSR $8aa5 -- "wait for a key", returns PETSCII in A

# Trampoline placed in the unused ROM filler. It runs the original far call
# with the screen pointed somewhere harmless, then clears the visible screen.
HELPER = bytes([
    0xad, 0x88, 0x02,        # lda $0288          ; KERNAL screen page
    0x48,                    # pha
    0xa9, 0xc0,              # lda #$c0           ; draw into $c000 instead
    0x8d, 0x88, 0x02,        # sta $0288
    0x20, 0x03, 0x9f,        # jsr $9f03          ; far call, bank 3...
    0x54, 0x80,              # .word $8054        ; ...entry $8054 (menu+probe)
    0x68,                    # pla
    0x8d, 0x88, 0x02,        # sta $0288          ; restore screen page
    0x20, 0x44, 0xe5,        # jsr $e544          ; clear screen
    0x60,                    # rts
])

PATCHES = [
    (0, FREE, HELPER),
    # replace the 5-byte far call with a 3-byte call to the trampoline
    (0, DRAW_CALL, bytes([0x20, FREE & 0xff, FREE >> 8, 0xea, 0xea])),
    # "the user pressed F7" ($88) instead of reading the keyboard
    (0, KEY_READ, bytes([0xa9, 0x88, 0xea])),
]


def chip_banks(data):
    """Map CRT bank number -> (file offset of payload, load address, size)."""
    off = struct.unpack('>I', data[16:20])[0]
    banks = {}
    while off < len(data):
        plen = struct.unpack('>I', data[off + 4:off + 8])[0]
        bank = struct.unpack('>H', data[off + 10:off + 12])[0]
        load = struct.unpack('>H', data[off + 12:off + 14])[0]
        size = struct.unpack('>H', data[off + 14:off + 16])[0]
        banks[bank] = (off + 16, load, size)
        off += plen
    return banks


def main(argv):
    if not 2 <= len(argv) <= 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    src = argv[1]
    dst = argv[2] if len(argv) > 2 else src.replace('.crt', '-auto.crt')

    data = bytearray(open(src, 'rb').read())
    if bytes(data[:16]) != b'C64 CARTRIDGE   ':
        sys.exit('%s: not a .crt image' % src)
    banks = chip_banks(bytes(data))

    for bank, addr, new in PATCHES:
        fo, load, size = banks[bank]
        if not load <= addr < load + size:
            sys.exit('$%04x outside bank %d' % (addr, bank))
        pos = fo + (addr - load)
        data[pos:pos + len(new)] = new

    open(dst, 'wb').write(bytes(data))
    print('wrote %s' % dst)


if __name__ == '__main__':
    main(sys.argv)
