#!/usr/bin/env python3
"""Fix RetroArch .slang shaders that fail naga (wgpu) validation.

RetroArch's normal pipeline (glslang + spirv-cross) accepts a couple of
constructs that naga rejects. This script rewrites two of them, preserving
semantics:

1. Matrix inter-stage varyings ("layout(...) out mat4 x;"). naga does not
   allow matrix types as entry-point I/O. When a matrix varying is written
   purely from uniforms/constants (the common "color profile" pattern), we
   delete the varying and move its computation into the fragment stage.

2. The modf() builtin. naga's SPIR-V frontend fails to register modf's special
   result type ("Type(MissingSpecialType)"). We inject local modf_() overloads
   (ip = trunc(x); return x - ip;) and rewrite modf( -> modf_( . This is exact:
   modf's integer part truncates toward zero, which is what trunc() does.

Both fixes are idempotent. Run with --dry-run to preview.

Usage:
    python3 fix_slang_naga.py [PATH ...] [--dry-run]

PATH may be a .slang file or a directory (searched recursively). Defaults to
./slang-shaders.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

MODF_HELPERS = """
// naga's SPIR-V frontend cannot lower the modf() builtin (its special result
// type is never registered), so call these exact equivalents instead.
float modf_(float x, out float ip) { ip = trunc(x); return x - ip; }
vec2  modf_(vec2  x, out vec2  ip) { ip = trunc(x); return x - ip; }
vec3  modf_(vec3  x, out vec3  ip) { ip = trunc(x); return x - ip; }
vec4  modf_(vec4  x, out vec4  ip) { ip = trunc(x); return x - ip; }
"""

# GLSL/slang built-in type keywords used to spot local variable declarations.
_TYPE_KW = (r"float|double|int|uint|bool|void"
            r"|[iub]?vec[234]|mat[234]|mat[234]x[234]"
            r"|sampler\w*|image\w*")


def split_stages(text: str):
    """Return (preamble, vertex, fragment) line-index ranges as slices of lines.

    Returns None when the file is not a two-stage vertex/fragment shader.
    """
    lines = text.splitlines(keepends=True)
    v = f = None
    for i, ln in enumerate(lines):
        s = ln.strip()
        if s.startswith("#pragma stage vertex"):
            if v is not None:
                return None  # more than one vertex stage; too unusual to touch
            v = i
        elif s.startswith("#pragma stage fragment"):
            if f is not None:
                return None
            f = i
    if v is None or f is None or not (v < f):
        return None
    return lines, v, f


# Matches a line/block comment (group 1) OR a modf() builtin call (group 2), so
# a single pass can rewrite the calls while leaving text inside comments alone.
_MODF_OR_COMMENT = re.compile(r"(//[^\n]*|/\*.*?\*/)|(\bmodf\s*\()", re.S)


def fix_modf(text: str) -> tuple[str, bool]:
    replaced = False

    def repl(m: re.Match) -> str:
        nonlocal replaced
        if m.group(1) is not None:  # a comment: leave untouched
            return m.group(0)
        replaced = True  # `\bmodf\s*\(` never matches an existing `modf_(`
        return "modf_("

    new = _MODF_OR_COMMENT.sub(repl, text)
    if not replaced:
        return text, False
    text = new
    # Inject the helper overloads once, right after the #version line.
    if "float modf_(" not in text:
        m = re.search(r"^#version[^\n]*\n", text, flags=re.M)
        insert_at = m.end() if m else 0
        text = text[:insert_at] + MODF_HELPERS + text[insert_at:]
    return text, True


def _vertex_local_names(vertex_lines: list[str]) -> set[str]:
    """Names that resolve only within the vertex stage.

    These are the per-vertex `in` attributes plus every variable declared
    inside the vertex stage (including `out` varyings and `main`'s locals). A
    moved statement referencing any of them cannot be relocated to the fragment
    stage, where those names do not exist.
    """
    names = set()
    decl_re = re.compile(r"\b(?:" + _TYPE_KW + r"|[A-Z]\w*)\s+(\w+)\s*(?:=|;|\[)")
    in_re = re.compile(r"\bin\s+\w+\s+(\w+)\s*;")
    for ln in vertex_lines:
        m = in_re.search(ln)
        if m:
            names.add(m.group(1))
        for m in decl_re.finditer(ln):
            names.add(m.group(1))
    return names


def fix_matrix_varyings(text: str) -> tuple[str, bool]:
    parsed = split_stages(text)
    if parsed is None:
        return text, False
    lines, v_idx, f_idx = parsed

    preamble = lines[:v_idx]
    vertex = lines[v_idx:f_idx]
    fragment = lines[f_idx:]

    out_re = re.compile(r"^\s*layout\s*\([^)]*\)\s*out\s+(mat[234])\s+(\w+)\s*;\s*$")
    changed = False

    # Collect candidate matrix varyings declared in the vertex stage.
    candidates = []
    for ln in vertex:
        m = out_re.match(ln)
        if m:
            candidates.append((m.group(1), m.group(2)))

    vertex_locals = _vertex_local_names(vertex)

    for mtype, name in candidates:
        out_decls = [i for i, ln in enumerate(vertex)
                     if out_re.match(ln) and out_re.match(ln).group(2) == name]
        in_re = re.compile(
            r"^\s*layout\s*\([^)]*\)\s*in\s+" + re.escape(mtype) + r"\s+"
            + re.escape(name) + r"\s*;\s*$")
        in_decls = [i for i, ln in enumerate(fragment) if in_re.match(ln)]
        # Only touch a plain, unique varying. Duplicates usually mean the decl
        # sits inside #if branches (e.g. scanline-classic) — leave those alone.
        if len(out_decls) != 1 or len(in_decls) != 1:
            continue

        assign_re = re.compile(r"\b" + re.escape(name) + r"\s*=(?!=)")
        assign_idx = [i for i, ln in enumerate(vertex) if assign_re.search(ln)]
        if not assign_idx:
            continue

        moved = [vertex[i] for i in assign_idx]
        # Every moved statement must be self-contained and depend only on
        # symbols that also exist in the fragment stage: no `gl_*` builtins and
        # no vertex-local names (inputs or locals). The varying itself is
        # allowed on the left-hand side.
        deps = set(vertex_locals) - {name}
        bad = False
        for ln in moved:
            if ";" not in ln or "gl_" in ln:
                bad = True
                break
            for dep in deps:
                if re.search(r"\b" + re.escape(dep) + r"\b", ln):
                    bad = True
                    break
            if bad:
                break
        if bad:
            continue

        # Rebuild the vertex stage without the out decl and the moved lines.
        drop = set(out_decls) | set(assign_idx)
        vertex = [ln for i, ln in enumerate(vertex) if i not in drop]

        # Rebuild the fragment stage: drop the in decl, then declare the matrix
        # locally and run the moved statements at the top of main().
        fragment = [ln for i, ln in enumerate(fragment) if i not in set(in_decls)]
        for i, ln in enumerate(fragment):
            if re.match(r"\s*void\s+main\s*\(\s*\)", ln):
                # Find the opening brace (same line or the next non-blank one).
                brace = i if "{" in ln else next(
                    (j for j in range(i + 1, len(fragment)) if "{" in fragment[j]),
                    None)
                if brace is None:
                    break
                block = [f"\t{mtype} {name};\n"]
                block += [m if m.endswith("\n") else m + "\n" for m in moved]
                fragment = fragment[:brace + 1] + block + fragment[brace + 1:]
                break
        changed = True

    if not changed:
        return text, False
    return "".join(preamble + vertex + fragment), True


def process(path: Path, dry_run: bool) -> bool:
    # latin-1 is a lossless byte<->char map, so round-tripping preserves any
    # non-UTF-8 bytes some of these shaders contain; our edits are ASCII-only.
    text = path.read_text(encoding="latin-1")
    new = text
    notes = []
    new, c1 = fix_matrix_varyings(new)
    if c1:
        notes.append("matrix-varying")
    new, c2 = fix_modf(new)
    if c2:
        notes.append("modf")
    if new == text:
        return False
    print(f"{'[dry-run] ' if dry_run else ''}{path}: {', '.join(notes)}")
    if not dry_run:
        path.write_text(new, encoding="latin-1")
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("paths", nargs="*", default=["slang-shaders"],
                    help="Files or directories to fix (default: slang-shaders)")
    ap.add_argument("--dry-run", action="store_true",
                    help="Report what would change without writing")
    args = ap.parse_args()

    files: list[Path] = []
    for p in args.paths:
        pp = Path(p)
        if pp.is_dir():
            files.extend(sorted(pp.rglob("*.slang")))
        elif pp.suffix == ".slang":
            files.append(pp)
        else:
            print(f"skipping non-.slang path: {pp}", file=sys.stderr)

    fixed = sum(process(f, args.dry_run) for f in files)
    print(f"\n{fixed} file(s) {'would be ' if args.dry_run else ''}fixed "
          f"out of {len(files)} scanned.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
