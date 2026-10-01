#!/usr/bin/env python3
"""Wrap a raw 64 KiB Retro Replay ROM dump in a .crt container.

Flash dumps (Turbo Action ROM, custom RR ROMs) ship as a bare 64 KiB image of
eight 8 KiB banks. VICE only attaches cartridges as .crt, so give the image the
64-byte CRT header of hardware type 36 (Retro Replay) and one CHIP packet per
bank, all loading at $8000.

Usage: rr_bin2crt.py <in.bin> [out.crt]
"""
import struct
import sys

BANK_SIZE = 0x2000
LOAD_ADDR = 0x8000
HW_TYPE = 36  # Retro Replay
CHIP_FLASH = 2


def main(argv):
    if not 2 <= len(argv) <= 3:
        sys.exit(__doc__.strip().splitlines()[-1])
    src = argv[1]
    dst = argv[2] if len(argv) > 2 else src.rsplit('.', 1)[0] + '.crt'

    rom = open(src, 'rb').read()
    if len(rom) % BANK_SIZE or not rom:
        sys.exit('%s: %d bytes is not a whole number of 8 KiB banks' % (src, len(rom)))

    out = bytearray(b'C64 CARTRIDGE   ')
    out += struct.pack('>IHHBB', 0x40, 0x0100, HW_TYPE, 0, 1)
    out += bytes(6)
    out += b'Retro Replay'.ljust(32, b'\0')

    for bank in range(len(rom) // BANK_SIZE):
        payload = rom[bank * BANK_SIZE:(bank + 1) * BANK_SIZE]
        out += b'CHIP'
        out += struct.pack('>IHHHH', 0x10 + BANK_SIZE, CHIP_FLASH, bank,
                           LOAD_ADDR, BANK_SIZE)
        out += payload

    open(dst, 'wb').write(bytes(out))
    print('wrote %s' % dst)


if __name__ == '__main__':
    main(sys.argv)
