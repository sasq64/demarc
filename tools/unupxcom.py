import sys, re, struct
# Port of the UPX COM stub (nrv2b, 16-bit bit buffer, e8/e9 filter).
data = open(sys.argv[1], 'rb').read()
code = data[:0x30]
mem = bytearray(0x10000)
mem[0x100:0x100+len(data)] = data
mov_cx, = struct.unpack_from('<H', data, 9); src, = struct.unpack_from('<H', data, 12)
dst, = struct.unpack_from('<H', data, 15); sub_si = struct.unpack_from('<b', data, 0x1c)[0]
# std rep movsb
for i in range(mov_cx): mem[dst - i] = mem[src - i]
si = dst - mov_cx - sub_si; di = 0x100
# filter count: find "b9 xx xx ac 2c e8" in the moved stub
m = re.search(rb'\xb9(..)\xac\x2c\xe8', data, re.S); fcount = struct.unpack('<H', m.group(1))[0]
bx = 0x8000; bp = 0
def getbit():
    global bx, si
    bx <<= 1; c = bx >> 16; bx &= 0xffff
    if bx: return c
    ax = mem[si] | mem[si+1] << 8; si += 2
    ax = (ax << 1) | c; c = ax >> 16; bx = ax & 0xffff
    return c
while True:
    while getbit(): mem[di] = mem[si]; si += 1; di += 1
    cx = 1
    while True:
        cx = ((cx << 1) | getbit()) & 0xffff
        b = getbit()
        if cx == 0: break
        if b: break
    if cx == 0: break
    cx -= 3
    if cx >= 0:
        ax = ((cx & 0xff) << 8) | mem[si]; si += 1
        bp = (~ax) & 0xffff
    cx = 0
    cx = (cx << 1) | getbit(); cx = (cx << 1) | getbit()
    if cx == 0:
        cx = 1
        while True:
            cx = ((cx << 1) | getbit()) & 0xffff
            if getbit(): break
        cx += 2
    cx += 1 + (1 if bp < 0xf300 else 0)
    s = (bp + di) & 0xffff
    for _ in range(cx): mem[di] = mem[s]; di += 1; s += 1
end = di
si = 0x100
for _ in range(fcount):
    while True:
        al = (mem[si] - 0xe8) & 0xff; si += 1
        if al <= 1: break
    w = mem[si] << 8 | mem[si+1]
    w = (w - si) & 0xffff
    mem[si] = w & 0xff; mem[si+1] = w >> 8; si += 2
open(sys.argv[2], 'wb').write(mem[0x100:end])
print(sys.argv[2], end - 0x100, 'bytes, filter', fcount)
