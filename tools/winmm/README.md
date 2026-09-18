# winmm.dll

A 32-bit `winmm.dll` whose export table has the same ordinals as the Windows
one, with every export jumping into wine's own builtin winmm (found through the
`WINEDLLDIR<n>` variables wine sets in every process).

Crinkler's range import finds one export by hash and takes the ones after it by
position in the export address table. Wine's winmm lacks `tid32Message` and the
other `*32Message` exports, so everything after them is off by one — Alcatraz'
"Prism break" calls `waveOutWrite` and lands in the export name table instead.

It also hands wine a copy of every `WAVEHDR` instead of the app's own. Windows
reads a header once at `waveOutWrite`, while wine reads it again on every
buffer it feeds, so Fairlight's "Uncovering Static" crashes: it plays its one
55 MB buffer from a header on the stack and then reuses that stack. The copy's
flags go back to the app's header, and callbacks get the app's pointer.

- `exports.txt` — `ordinal name stdcall-arg-bytes`, ordinals 2–194 taken from
  Windows 10 (10240 and 28000 are identical), then the names only wine has.
  Exports wine does not implement return 0.
- `winmm.c` — loads wine's winmm and fills in the jump slots.
- `build.py` — generates the thunks and `.def`, builds with clang/lld-link.

`just winmm` rebuilds `system/win/winmm.dll`. `scripts/setup-wine.sh` copies it into
the prefix's `syswow64`, and demarc sets `winmm=n,b`.
