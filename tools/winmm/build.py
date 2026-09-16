#!/usr/bin/env python3
"""Build winmm.dll from exports.txt and winmm.c with clang and lld-link.

Usage: build.py <out.dll>
"""

import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TARGET = "i686-pc-windows-msvc"

KERNEL32 = ["LoadLibraryW@4", "GetProcAddress@8", "GetEnvironmentVariableW@12",
            "DisableThreadLibraryCalls@4"]


def read_exports():
    rows = []
    with open(os.path.join(HERE, "exports.txt")) as f:
        for line in f:
            parts = line.split()
            if not parts or parts[0].startswith("#"):
                continue
            ordinal, name, arg_bytes = int(parts[0]), parts[1], int(parts[2])
            rows.append((ordinal, name, arg_bytes, "noname" in parts[3:]))
    return rows


def thunks(rows):
    out = [".text"]
    for i, (_, _, arg_bytes, _) in enumerate(rows):
        out += [f".globl _t{i}", f"_t{i}:", f"    jmp *_slots+{4 * i}",
                f"_f{i}:", "    xorl %eax, %eax", f"    ret ${arg_bytes}"]
    out += [".data", ".globl _slots", "_slots:"]
    out += [f"    .long _f{i}" for i in range(len(rows))]
    out += [".section .rdata,\"dr\"", ".globl _export_count", "_export_count:",
            f"    .long {len(rows)}", ".globl _names", "_names:"]
    out += [f"    .long _n{i}" for i in range(len(rows))]
    out += [f"_n{i}: .asciz \"{name}\"" for i, (_, name, _, _) in enumerate(rows)]
    return "\n".join(out) + "\n"


def def_file(rows):
    out = ["LIBRARY winmm.dll", "EXPORTS"]
    for i, (ordinal, name, _, noname) in enumerate(rows):
        out.append(f"    {'_noname' + str(ordinal) if noname else name}=t{i} @{ordinal}"
                   + (" NONAME" if noname else ""))
    return "\n".join(out) + "\n"


def main():
    out_dll = os.path.abspath(sys.argv[1])
    rows = read_exports()
    with tempfile.TemporaryDirectory() as tmp:
        def write(name, text):
            path = os.path.join(tmp, name)
            with open(path, "w") as f:
                f.write(text)
            return path

        def run(*cmd):
            subprocess.run(cmd, check=True, cwd=tmp)

        write("thunks.S", thunks(rows))
        write("winmm.def", def_file(rows))
        write("kernel32.def", "LIBRARY kernel32.dll\nEXPORTS\n" + "\n".join(KERNEL32) + "\n")
        cflags = [f"--target={TARGET}", "-O2", "-ffreestanding", "-fno-builtin", "-c"]
        run("clang", *cflags, "thunks.S", "-o", "thunks.obj")
        run("clang", *cflags, os.path.join(HERE, "winmm.c"), "-o", "winmm.obj")
        run("llvm-dlltool", "-m", "i386", "-k", "-d", "kernel32.def", "-l", "kernel32.lib")
        run("lld-link", "/nologo", "/dll", "/machine:x86", "/nodefaultlib", "/entry:DllMain",
            "/safeseh:no", "/Brepro", "/noimplib", "/def:winmm.def", f"/out:{out_dll}", "thunks.obj", "winmm.obj",
            "kernel32.lib")


if __name__ == "__main__":
    main()
