# xDMS 1.3 — DMS archive unpacker

Public domain, written by Andre Rodrigues de la Rocha <adlroc@usa.net>.

These are the sources amiberry carries in `src/archivers/dms`, copied here so
demarc can unpack a `.dms` release into the floppy image inside it (see
`src/newsys/dms.rs`, and `--unadf` in `src/newsys/amiga.rs`).

Three changes were made to what was copied:

* The `.cpp` extension was dropped. The code is plain C, and building it as C
  keeps the binary off libstdc++.
* `pfile.c` reads and writes stdio streams instead of amiberry's `struct
  zfile`, does not log, and does not keep the banner / FILEID.DIZ / fake boot
  block streams amiberry shows the user. Its header comment lists this too.

* `u_heavy.c` rejects Huffman tree headers that overflow `c_len` / `pt_len`
  and stops a match copy at the end of the track. Upstream has neither check,
  and `pfile.c`'s password search feeds every wrong key's garbage through
  `Unpack_HEAVY`, so an encrypted Heavy-mode archive (e.g.
  Triads-NewSolutionsCompB.dms) reliably hit a tree size of 511, which the
  compiler turns into a 64 KiB `memset` off the end of `.bss`. Lines marked
  `demarc:`.

Everything else — the other decrunchers, the bit reader, the Huffman table
builder, the CRC and checksum code — is byte-for-byte upstream, so a fix from
amiberry can be dropped straight in (keep the `u_heavy.c` checks).
