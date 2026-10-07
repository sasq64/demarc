#!/usr/bin/env python3
"""Build kernelbase.dll from wine's i386 kernelbase.dll and kernelbase.c with
clang and lld-link.

Usage: build.py <wine kernelbase.dll> <out.dll>
"""

import os
import struct
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = "i686-pc-windows-msvc"

NTDLL = ["LdrLoadDll@16", "LdrGetProcedureAddress@16", "RtlQueryEnvironmentVariable_U@12",
         "LdrDisableThreadCalloutsForDll@4"]


def read_exports(path):
    """(ordinal, name or None) for every export of the PE at `path`."""
    d = open(path, "rb").read()
    pe = struct.unpack_from("<I", d, 0x3C)[0]
    opt = pe + 24
    count = struct.unpack_from("<H", d, pe + 6)[0]
    opt_size = struct.unpack_from("<H", d, pe + 20)[0]
    sections = [struct.unpack_from("<8sIIII", d, opt + opt_size + 40 * i) for i in range(count)]

    def off(rva):
        for _, vsize, va, rsize, raw in sections:
            if va <= rva < va + max(vsize, rsize):
                return rva - va + raw
        raise ValueError(hex(rva))

    def cstr(rva):
        o = off(rva)
        return d[o:d.index(b"\0", o)].decode()

    export_rva = struct.unpack_from("<I", d, opt + 96)[0]
    (base, n_funcs, n_names, funcs, names, ords) = struct.unpack_from(
        "<IIIIII", d, off(export_rva) + 16)
    by_index = {}
    for i in range(n_names):
        index = struct.unpack_from("<H", d, off(ords) + 2 * i)[0]
        by_index[index] = cstr(struct.unpack_from("<I", d, off(names) + 4 * i)[0])
    rows = []
    for i in range(n_funcs):
        if struct.unpack_from("<I", d, off(funcs) + 4 * i)[0]:
            rows.append((base + i, by_index.get(i)))
    return rows


def thunks(rows):
    out = [".text"]
    for i in range(len(rows)):
        out += [f".globl _t{i}", f"_t{i}:", f"    jmp *_slots+{4 * i}"]
    out += ["_stub:", "    xorl %eax, %eax", "    ret"]
    out += [".data", ".globl _slots", "_slots:"]
    out += ["    .long _stub" for _ in rows]
    out += [".section .rdata,\"dr\"", ".globl _export_count", "_export_count:",
            f"    .long {len(rows)}", ".globl _ordinals", "_ordinals:"]
    out += [f"    .short {ordinal}" for ordinal, _ in rows]
    out += [".p2align 2", ".globl _names", "_names:"]
    out += [f"    .long _n{i}" if name else "    .long 0" for i, (_, name) in enumerate(rows)]
    out += [f"_n{i}: .asciz \"{name}\"" for i, (_, name) in enumerate(rows) if name]
    return "\n".join(out) + "\n"


def def_file(rows):
    out = ["LIBRARY kernelbase.dll", "EXPORTS"]
    for i, (ordinal, name) in enumerate(rows):
        out.append(f"    {name}=t{i} @{ordinal}" if name
                   else f"    _noname{ordinal}=t{i} @{ordinal} NONAME")
    return "\n".join(out) + "\n"


def main():
    rows = read_exports(sys.argv[1])
    out_dll = os.path.abspath(sys.argv[2])
    with tempfile.TemporaryDirectory() as tmp:
        def write(name, text):
            with open(os.path.join(tmp, name), "w") as f:
                f.write(text)

        def run(*cmd):
            subprocess.run(cmd, check=True, cwd=tmp)

        write("thunks.S", thunks(rows))
        write("kernelbase.def", def_file(rows))
        write("ntdll.def", "LIBRARY ntdll.dll\nEXPORTS\n" + "\n".join(NTDLL) + "\n")
        cflags = [f"--target={TARGET}", "-O2", "-ffreestanding", "-fno-builtin", "-c"]
        run("clang", *cflags, "thunks.S", "-o", "thunks.obj")
        run("clang", *cflags, os.path.join(HERE, "kernelbase.c"), "-o", "kernelbase.obj")
        run("llvm-dlltool", "-m", "i386", "-k", "-d", "ntdll.def", "-l", "ntdll.lib")
        run("lld-link", "/nologo", "/dll", "/machine:x86", "/nodefaultlib", "/entry:DllMain",
            "/safeseh:no", "/Brepro", "/noimplib", "/def:kernelbase.def", f"/out:{out_dll}",
            "thunks.obj", "kernelbase.obj", "ntdll.lib")


if __name__ == "__main__":
    main()
