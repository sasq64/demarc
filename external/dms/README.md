# xDMS 1.3 — DMS archive unpacker

Public domain, written by Andre Rodrigues de la Rocha <adlroc@usa.net>.

These are the sources amiberry carries in `src/archivers/dms`, copied here so
demarc can unpack a `.dms` release into the floppy image inside it (see
`src/newsys/dms.rs`, and `--unadf` in `src/newsys/amiga.rs`).

Two changes were made to what was copied:

* The `.cpp` extension was dropped. The code is plain C, and building it as C
  keeps the binary off libstdc++.
* `pfile.c` reads and writes stdio streams instead of amiberry's `struct
  zfile`, does not log, and does not keep the banner / FILEID.DIZ / fake boot
  block streams amiberry shows the user. Its header comment lists this too.

Everything else — the decrunchers (`u_*.c`), the bit reader, the Huffman table
builder, the CRC and checksum code — is byte-for-byte upstream, so a fix from
amiberry can be dropped straight in.
