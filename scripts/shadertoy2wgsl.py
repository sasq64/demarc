#!/usr/bin/env python3
"""Converts a Shadertoy fragment shader (one defining mainImage) into a WGSL
shader for a demarc theme ([screen] / [panel] shader).

usage: shadertoy2wgsl.py <in.glsl> [out.wgsl]

The output relies on the prelude demarc adds to theme shaders (`ui`, `tex`,
`samp`, `FullscreenVertexOutput`). Every iChannel is the theme texture.
"""

import re
import sys
from dataclasses import dataclass


class ConvertError(Exception):
    def __init__(self, line, msg):
        super().__init__(f"{line}: {msg}" if line else msg)


# ------------------------------------------------------------------ lexer


@dataclass
class Tok:
    kind: str  # id, num, op, eof
    val: str
    line: int


TOKEN_RE = re.compile(
    r"""
    (?P<ws>\s+)
  | (?P<num>0[xX][0-9a-fA-F]+[uU]?|(?:\d+\.\d*|\.\d+|\d+)(?:[eE][+-]?\d+)?[fFuU]?)
  | (?P<id>[A-Za-z_]\w*)
  | (?P<op><<=|>>=|\+\+|--|<<|>>|<=|>=|==|!=|&&|\|\||\^\^|[-+*/%&|^]=|\#\#|[-+*/%<>=!~&|^?:;,.(){}\[\]\#])
""",
    re.X,
)


def lex(text, line):
    toks = []
    pos = 0
    while pos < len(text):
        m = TOKEN_RE.match(text, pos)
        if not m:
            raise ConvertError(line, f"unexpected character {text[pos]!r}")
        if m.lastgroup != "ws":
            toks.append(Tok(m.lastgroup, m.group(), line))
        pos = m.end()
    return toks


def strip_comments(src):
    def blank(m):
        return "\n" * m.group().count("\n") or " "

    return re.sub(r"//[^\n]*|/\*.*?\*/", blank, src, flags=re.S)


# ----------------------------------------------------------- preprocessor

SCALARS = {"float": "f32", "int": "i32", "uint": "u32", "bool": "bool"}
TYPE_NAMES = (
    set(SCALARS)
    | {f"{p}vec{n}" for p in ("", "i", "u", "b") for n in (2, 3, 4)}
    | {f"mat{c}" for c in (2, 3, 4)}
    | {f"mat{c}x{r}" for c in (2, 3, 4) for r in (2, 3, 4)}
    | {"void", "sampler2D"}
)


@dataclass
class Macro:
    params: list | None
    body: list


def matching(toks, i):
    """Index of the bracket closing the one at toks[i]."""
    depth = 0
    for j in range(i, len(toks)):
        if toks[j].val in "([{":
            depth += 1
        elif toks[j].val in ")]}":
            depth -= 1
            if depth == 0:
                return j
    raise ConvertError(toks[i].line, "unbalanced brackets")


class Preprocessor:
    def __init__(self):
        self.macros = {}
        # Object-like macros holding a constant: emitted as WGSL consts
        # instead of being expanded.
        self.consts = {}

    def run(self, src):
        lines = strip_comments(src).split("\n")
        out = []
        stack = []  # [parent active, a branch was taken]
        active = True
        i = 0
        while i < len(lines):
            lineno = i + 1
            text = lines[i]
            while text.endswith("\\") and i + 1 < len(lines):
                i += 1
                text = text[:-1] + " " + lines[i]
            i += 1
            s = text.strip()
            if not s.startswith("#"):
                if active:
                    out += lex(text, lineno)
                continue
            m = re.match(r"#\s*(\w*)\s*(.*)", s)
            d, rest = m.group(1), m.group(2).strip()
            if d in ("if", "ifdef", "ifndef"):
                if not active:
                    stack.append([False, True])
                    continue
                if d == "if":
                    cond = self.eval_if(rest, lineno)
                else:
                    cond = (rest.split()[0] in self.macros) == (d == "ifdef")
                stack.append([True, cond])
                active = cond
            elif d == "elif":
                parent, taken = stack[-1]
                active = parent and not taken and self.eval_if(rest, lineno)
                stack[-1][1] = taken or active
            elif d == "else":
                parent, taken = stack[-1]
                active = parent and not taken
                stack[-1][1] = True
            elif d == "endif":
                active = stack.pop()[0]
            elif not active:
                pass
            elif d == "define":
                self.define(rest, lineno)
            elif d == "undef":
                self.macros.pop(rest, None)
                self.consts.pop(rest, None)
            elif d == "error":
                raise ConvertError(lineno, f"#error {rest}")
        return self.expand(out)

    def define(self, rest, line):
        m = re.match(r"([A-Za-z_]\w*)(\(([^)]*)\))?(.*)$", rest)
        if not m:
            raise ConvertError(line, "bad #define")
        name = m.group(1)
        params = None
        if m.group(2):
            params = [p.strip() for p in m.group(3).split(",") if p.strip()]
        body = lex(m.group(4), line)
        self.macros[name] = Macro(params, body)
        self.consts.pop(name, None)
        if params is None and self.is_const(body):
            self.consts[name] = body

    def is_const(self, body):
        for t in body:
            if t.kind == "id" and t.val not in self.consts and t.val not in TYPE_NAMES:
                return False
            if t.kind == "op" and t.val not in "+-*/(),":
                return False
        if len(body) == 1:
            return body[0].kind == "num" or body[0].val in self.consts
        if len(body) == 2:
            return body[0].val in "+-" and body[1].kind == "num"
        start = 1 if body[0].val in TYPE_NAMES else 0
        return body[start].val == "(" and matching(body, start) == len(body) - 1

    def expand(self, toks, hide=frozenset(), keep_consts=True):
        out = []
        i = 0
        while i < len(toks):
            t = toks[i]
            m = self.macros.get(t.val) if t.kind == "id" else None
            if m is None or t.val in hide or (keep_consts and t.val in self.consts):
                out.append(t)
                i += 1
                continue
            if m.params is None:
                body = [Tok(b.kind, b.val, t.line) for b in m.body]
                out += self.expand(body, hide | {t.val}, keep_consts)
                i += 1
                continue
            if i + 1 >= len(toks) or toks[i + 1].val != "(":
                out.append(t)
                i += 1
                continue
            end = matching(toks, i + 1)
            args = self.split_args(toks[i + 2 : end])
            if len(args) != len(m.params) and not (m.params == [] and args == [[]]):
                raise ConvertError(t.line, f"macro {t.val} takes {len(m.params)} arguments")
            raw = dict(zip(m.params, args))
            expanded = {p: self.expand(a, hide, keep_consts) for p, a in raw.items()}
            body = self.substitute(m.body, raw, expanded, t.line)
            out += self.expand(body, hide | {t.val}, keep_consts)
            i = end + 1
        return out

    @staticmethod
    def split_args(toks):
        args = [[]]
        depth = 0
        for t in toks:
            if t.val in "([{":
                depth += 1
            elif t.val in ")]}":
                depth -= 1
            if t.val == "," and depth == 0:
                args.append([])
            else:
                args[-1].append(t)
        return args

    @staticmethod
    def substitute(body, raw, expanded, line):
        out = []
        i = 0
        while i < len(body):
            b = body[i]
            if b.val == "##" and out and i + 1 < len(body):
                nxt = body[i + 1]
                right = raw.get(nxt.val, [nxt]) if nxt.kind == "id" else [nxt]
                left = out.pop()
                joined = lex(left.val + (right[0].val if right else ""), line)
                out += joined + [Tok(r.kind, r.val, line) for r in right[1:]]
                i += 2
                continue
            if b.kind == "id" and b.val in raw:
                glued = i + 1 < len(body) and body[i + 1].val == "##"
                out += [Tok(a.kind, a.val, line) for a in (raw if glued else expanded)[b.val]]
            elif b.val != "#":
                out.append(Tok(b.kind, b.val, line))
            i += 1
        return out

    def eval_if(self, rest, line):
        toks = lex(rest, line)
        out = []
        i = 0
        while i < len(toks):
            if toks[i].val == "defined":
                if toks[i + 1].val == "(":
                    name, i = toks[i + 2].val, i + 4
                else:
                    name, i = toks[i + 1].val, i + 2
                out.append(Tok("num", "1" if name in self.macros else "0", line))
            else:
                out.append(toks[i])
                i += 1
        py = []
        for t in self.expand(out, keep_consts=False):
            if t.kind == "num":
                py.append(t.val.rstrip("fFuU"))
            elif t.kind == "id":
                py.append("1" if t.val == "true" else "0")
            else:
                py.append({"&&": " and ", "||": " or ", "!": " not ", "/": "//"}.get(t.val, t.val))
        try:
            return bool(eval("".join(py), {}))
        except Exception:
            raise ConvertError(line, f"can't evaluate #if {rest}")


# ----------------------------------------------------------------- parser


class Node:
    def __init__(self, kind, line, **kw):
        self.kind = kind
        self.line = line
        self.__dict__.update(kw)


AUTO = "auto"  # array size taken from the initializer
QUALIFIERS = {"const", "in", "out", "inout", "highp", "mediump", "lowp", "flat", "smooth", "invariant", "precise"}
BINPREC = {
    "||": 1, "^^": 2, "&&": 3, "|": 4, "^": 5, "&": 6, "==": 7, "!=": 7,
    "<": 8, ">": 8, "<=": 8, ">=": 8, "<<": 9, ">>": 9, "+": 10, "-": 10, "*": 11, "/": 11, "%": 11,
}
ASSIGN_OPS = {"=", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<=", ">>="}


class Parser:
    def __init__(self, toks, structs=()):
        last = toks[-1].line if toks else 0
        self.toks = toks + [Tok("eof", "", last)]
        self.i = 0
        self.structs = set(structs)

    def peek(self, k=0):
        return self.toks[min(self.i + k, len(self.toks) - 1)]

    def next(self):
        t = self.toks[self.i]
        if t.kind != "eof":
            self.i += 1
        return t

    def at(self, val):
        t = self.peek()
        return t.kind == "op" and t.val == val or t.kind == "id" and t.val == val

    def accept(self, val):
        if self.at(val):
            return self.next()
        return None

    def expect(self, val):
        t = self.next()
        if t.val != val or t.kind == "num":
            raise ConvertError(t.line, f"expected {val!r}, got {t.val or 'end of file'!r}")
        return t

    def expect_id(self):
        t = self.next()
        if t.kind != "id":
            raise ConvertError(t.line, f"expected a name, got {t.val!r}")
        return t.val

    def is_type(self, t):
        return t.kind == "id" and (t.val in TYPE_NAMES or t.val in self.structs)

    def is_decl_start(self):
        j = self.i
        while self.toks[j].val in QUALIFIERS:
            j += 1
        if not self.is_type(self.toks[j]):
            return False
        j += 1
        if self.toks[j].val == "[":
            j = matching(self.toks, j) + 1
        return self.toks[j].kind == "id"

    def skip_to(self, val):
        while not self.accept(val):
            if self.next().kind == "eof":
                return

    # --- declarations

    def parse_unit(self):
        items = []
        while self.peek().kind != "eof":
            if self.accept(";"):
                continue
            if self.accept("precision"):
                self.skip_to(";")
                continue
            if self.at("struct"):
                items.append(self.parse_struct())
                continue
            line = self.peek().line
            quals = self.parse_quals()
            ty, arr = self.parse_type()
            name = self.expect_id()
            if self.accept("("):
                items.append(self.parse_function(ty, name, line))
            else:
                items.append(self.parse_decl_rest(quals, ty, arr, name, line))
        return items

    def parse_quals(self):
        quals = set()
        while self.peek().val in QUALIFIERS:
            quals.add(self.next().val)
        if self.at("uniform"):
            raise ConvertError(self.peek().line, "uniforms other than Shadertoy's are not supported")
        return quals

    def parse_type(self):
        t = self.next()
        if not self.is_type(t):
            raise ConvertError(t.line, f"expected a type, got {t.val!r}")
        return t.val, self.parse_array_suffix()

    def parse_array_suffix(self):
        if not self.accept("["):
            return None
        size = AUTO if self.at("]") else self.parse_assign()
        self.expect("]")
        return size

    def parse_decl_rest(self, quals, ty, arr, name, line):
        items = []
        while True:
            a = self.parse_array_suffix() or arr
            init = self.parse_assign() if self.accept("=") else None
            items.append((name, a, init))
            if not self.accept(","):
                break
            name = self.expect_id()
        self.expect(";")
        return Node("decl", line, quals=quals, ty=ty, items=items)

    def parse_function(self, ret, name, line):
        params = []
        if self.at("void") and self.peek(1).val == ")":
            self.next()
        while not self.accept(")"):
            quals = self.parse_quals()
            ty, arr = self.parse_type()
            pname = self.expect_id() if self.peek().kind == "id" else None
            arr = self.parse_array_suffix() or arr
            if arr is not None:
                raise ConvertError(line, "array parameters are not supported")
            params.append((quals, ty, pname))
            if not self.at(")"):
                self.expect(",")
        body = None if self.accept(";") else self.parse_block()
        return Node("func", line, ret=ret, name=name, params=params, body=body)

    def parse_struct(self):
        line = self.expect("struct").line
        name = self.expect_id()
        self.structs.add(name)
        self.expect("{")
        fields = []
        while not self.accept("}"):
            self.parse_quals()
            ty, arr = self.parse_type()
            while True:
                fname = self.expect_id()
                fields.append((ty, fname, self.parse_array_suffix() or arr))
                if not self.accept(","):
                    break
            self.expect(";")
        if not self.at(";"):
            raise ConvertError(line, "declare struct variables separately")
        self.expect(";")
        return Node("struct", line, name=name, fields=fields)

    # --- statements

    def parse_block(self):
        line = self.expect("{").line
        stmts = []
        while not self.accept("}"):
            if self.peek().kind == "eof":
                raise ConvertError(line, "unclosed block")
            stmts.append(self.parse_stmt())
        return Node("block", line, stmts=stmts)

    def parse_stmt(self):
        t = self.peek()
        line, v = t.line, t.val if t.kind != "num" else None
        if v == "{":
            return self.parse_block()
        if v == ";":
            self.next()
            return Node("empty", line)
        if v == "if":
            self.next()
            self.expect("(")
            cond = self.parse_expr()
            self.expect(")")
            then = self.parse_stmt()
            els = self.parse_stmt() if self.accept("else") else None
            return Node("if", line, cond=cond, then=then, els=els)
        if v == "for":
            self.next()
            self.expect("(")
            init = None if self.accept(";") else self.parse_simple_stmt()
            cond = None if self.at(";") else self.parse_expr()
            self.expect(";")
            step = None if self.at(")") else self.parse_expr()
            self.expect(")")
            return Node("for", line, init=init, cond=cond, step=step, body=self.parse_stmt())
        if v == "while":
            self.next()
            self.expect("(")
            cond = self.parse_expr()
            self.expect(")")
            return Node("while", line, cond=cond, body=self.parse_stmt())
        if v == "do":
            self.next()
            body = self.parse_stmt()
            self.expect("while")
            self.expect("(")
            cond = self.parse_expr()
            self.expect(")")
            self.expect(";")
            return Node("dowhile", line, cond=cond, body=body)
        if v == "return":
            self.next()
            e = None if self.at(";") else self.parse_expr()
            self.expect(";")
            return Node("return", line, e=e)
        if v in ("break", "continue", "discard"):
            self.next()
            self.expect(";")
            return Node(v, line)
        if v == "switch":
            return self.parse_switch()
        if v == "precision":
            self.skip_to(";")
            return Node("empty", line)
        return self.parse_simple_stmt()

    def parse_simple_stmt(self):
        line = self.peek().line
        if self.is_decl_start():
            quals = self.parse_quals()
            ty, arr = self.parse_type()
            return self.parse_decl_rest(quals, ty, arr, self.expect_id(), line)
        e = self.parse_expr()
        self.expect(";")
        return Node("expr", line, e=e)

    def parse_switch(self):
        line = self.next().line
        self.expect("(")
        e = self.parse_expr()
        self.expect(")")
        self.expect("{")
        cases = []
        while not self.accept("}"):
            if self.accept("case") or self.at("default"):
                if self.accept("default"):
                    label = "default"
                else:
                    label = self.parse_ternary()
                self.expect(":")
                if cases and not cases[-1][1]:
                    cases[-1][0].append(label)
                else:
                    cases.append([[label], []])
            elif not cases:
                raise ConvertError(line, "statement before the first case")
            else:
                cases[-1][1].append(self.parse_stmt())
        return Node("switch", line, e=e, cases=cases)

    # --- expressions

    def parse_expr(self):
        e = self.parse_assign()
        while self.at(","):
            line = self.next().line
            e = Node("comma", line, a=e, b=self.parse_assign())
        return e

    def parse_assign(self):
        e = self.parse_ternary()
        t = self.peek()
        if t.kind == "op" and t.val in ASSIGN_OPS:
            self.next()
            return Node("assign", t.line, op=t.val, target=e, value=self.parse_assign())
        return e

    def parse_ternary(self):
        c = self.parse_binary(1)
        if self.at("?"):
            line = self.next().line
            a = self.parse_expr()
            self.expect(":")
            return Node("ternary", line, cond=c, a=a, b=self.parse_assign())
        return c

    def parse_binary(self, minp):
        lhs = self.parse_unary()
        while True:
            t = self.peek()
            p = BINPREC.get(t.val) if t.kind == "op" else None
            if p is None or p < minp:
                return lhs
            self.next()
            lhs = Node("binary", t.line, op=t.val, a=lhs, b=self.parse_binary(p + 1))

    def parse_unary(self):
        t = self.peek()
        if t.kind == "op" and t.val in ("-", "+", "!", "~", "++", "--"):
            self.next()
            return Node("unary", t.line, op=t.val, e=self.parse_unary())
        return self.parse_postfix()

    def parse_postfix(self):
        e = self.parse_primary()
        while True:
            t = self.peek()
            if self.accept("["):
                idx = self.parse_expr()
                self.expect("]")
                e = Node("index", t.line, obj=e, idx=idx)
            elif self.accept("."):
                e = Node("member", t.line, obj=e, name=self.expect_id())
            elif t.kind == "op" and t.val in ("++", "--"):
                self.next()
                e = Node("postfix", t.line, op=t.val, e=e)
            else:
                return e

    def parse_primary(self):
        t = self.next()
        if t.kind == "num":
            return Node("num", t.line, text=t.val)
        if t.val == "(" and t.kind == "op":
            e = self.parse_expr()
            self.expect(")")
            return e
        if t.kind == "id":
            if self.is_type(t):
                arr = self.parse_array_suffix()
                self.expect("(")
                return Node("call", t.line, name=t.val, args=self.parse_args(), ctor=True, arr=arr)
            if self.accept("("):
                return Node("call", t.line, name=t.val, args=self.parse_args(), ctor=False, arr=None)
            return Node("ident", t.line, name=t.val)
        raise ConvertError(t.line, f"unexpected {t.val or 'end of file'!r}")

    def parse_args(self):
        args = []
        if self.at("void") and self.peek(1).val == ")":
            self.next()
        while not self.accept(")"):
            args.append(self.parse_assign())
            if not self.at(")"):
                self.expect(",")
        return args


# ------------------------------------------------------------------ types

VEC_RE = re.compile(r"([iub]?)vec([234])$")
MAT_RE = re.compile(r"mat([234])(?:x([234]))?$")
PREFIX_BASE = {"": "float", "i": "int", "u": "uint", "b": "bool"}
BASE_PREFIX = {v: k for k, v in PREFIX_BASE.items()}
BASE_SUFFIX = {"float": "f", "int": "i", "uint": "u"}


@dataclass(frozen=True)
class Arr:
    elem: str
    size: str


def ncomp(t):
    if t in SCALARS:
        return 1
    m = isinstance(t, str) and VEC_RE.match(t)
    return int(m.group(2)) if m else 0


def base(t):
    if t in SCALARS:
        return t
    if isinstance(t, str):
        m = VEC_RE.match(t)
        if m:
            return PREFIX_BASE[m.group(1)]
        if MAT_RE.match(t):
            return "float"
    return None


def vec(b, n):
    return b if n == 1 else f"{BASE_PREFIX[b]}vec{n}"


def is_mat(t):
    return isinstance(t, str) and MAT_RE.match(t) is not None


def mat_dims(t):
    m = MAT_RE.match(t)
    return int(m.group(1)), int(m.group(2) or m.group(1))


def mat_name(c, r):
    return f"mat{c}" if c == r else f"mat{c}x{r}"


# ---------------------------------------------------------------- emitter

PRIMARY, UNARY = 100, 50
REL = {"<", ">", "<=", ">=", "==", "!="}
LOGIC = {"&&", "||"}
BITWISE = {"&", "|", "^"}
SHIFT = {"<<", ">>"}

WGSL_RESERVED = set(
    """alias break case const const_assert continue continuing default diagnostic discard else
    enable false fn for if let loop override requires return struct switch true var while NULL
    Self abstract active alignas alignof as asm asm_fragment async attribute auto await become
    binding_array cast catch class co_await co_return co_yield coherent column_major common
    compile compile_fragment concept const_cast consteval constexpr constinit crate debugger
    decltype delete demote demote_to_helper do dynamic_cast enum explicit export extends extern
    external fallthrough filter final finally friend from fxgroup get goto groupshared highp impl
    implements import inline instanceof interface layout lowp macro macro_rules match mediump meta
    mod module move mut mutable namespace new nil noexcept noinline nointerpolation noperspective
    null nullptr of operator package packoffset partition pass patch pixelfragment precise
    precision premerge priv protected pub public readonly ref regardless register
    reinterpret_cast require resource restrict self set shared sizeof smooth snorm static
    static_assert static_cast std subroutine super target template this thread_local throw trait
    try type typedef typeid typename typeof union unless unorm unsafe unsized use using varying
    virtual volatile wgsl where with writeonly yield
    f32 i32 u32 f16 array ptr atomic sampler vec2f vec3f vec4f vec2i vec3i vec4i vec2u vec3u vec4u
    mat2x2f mat3x3f mat4x4f
    ui tex samp fragment frag_coord FullscreenVertexOutput Ui
    select atan2 inverseSqrt dpdx dpdy faceForward bitcast saturate textureSampleLevel
    textureSampleGrad textureLoad textureDimensions""".split()
)


def rename(name):
    if name in WGSL_RESERVED or name.startswith("__") or name.startswith("mod_"):
        return name + "_"
    return name


@dataclass
class R:
    """An emitted expression."""

    code: str
    ty: object
    prec: int = PRIMARY
    op: str | None = None
    abstract: bool = False  # a WGSL abstract (literal) number


@dataclass
class Sym:
    code: str
    ty: object
    kind: str  # var, let, const, global, ptr, sampler
    abstract: bool = False


@dataclass
class Func:
    node: Node
    wname: str
    params: list  # (kind, type, name); kind is in, out, inout or sampler
    ret: str


UNIFORMS = {
    "iTime": ("ui.time", "float"),
    "iGlobalTime": ("ui.time", "float"),
    "iTimeDelta": ("(1.0 / 60.0)", "float"),
    "iFrameRate": ("60.0", "float"),
    "iFrame": ("i32(ui.time * 60.0)", "int"),
    "iMouse": ("vec4f(0.0)", "vec4"),
    "iDate": ("vec4f(0.0, 0.0, 0.0, ui.time)", "vec4"),
    "iSampleRate": ("44100.0", "float"),
    "iResolution": ("vec3f(ui.resolution, 1.0)", "vec3"),
    **{f"iChannel{i}": ("tex", "sampler2D") for i in range(4)},
}

# GLSL builtins that keep the type of their (first) argument.
SAME_TYPE = {
    "sin", "cos", "tan", "asin", "acos", "sinh", "cosh", "tanh", "asinh", "acosh", "atanh",
    "exp", "exp2", "log", "log2", "sqrt", "abs", "sign", "floor", "ceil", "fract", "trunc",
    "round", "radians", "degrees", "normalize", "fwidth", "transpose", "reflect", "refract",
    "inversesqrt", "dFdx", "dFdy", "roundEven", "faceforward",
}
# GLSL builtins whose scalar arguments WGSL wants splatted to the vector type.
SPLAT = {"min", "max", "clamp", "pow", "step", "smoothstep", "mod", "atan"}
RENAMES = {
    "inversesqrt": "inverseSqrt", "dFdx": "dpdx", "dFdy": "dpdy", "roundEven": "round",
    "faceforward": "faceForward",
}
COMPARE = {"lessThan": "<", "lessThanEqual": "<=", "greaterThan": ">", "greaterThanEqual": ">=",
           "equal": "==", "notEqual": "!="}
BITCASTS = {"floatBitsToUint": "uint", "floatBitsToInt": "int", "uintBitsToFloat": "float",
            "intBitsToFloat": "float"}
TEXTURE_FNS = {"texture", "texture2D", "textureLod", "texture2DLod", "texelFetch", "textureSize",
               "textureGrad"}


def walk(node):
    if isinstance(node, Node):
        yield node
        for v in node.__dict__.values():
            yield from walk(v)
    elif isinstance(node, (list, tuple)):
        for v in node:
            yield from walk(v)


def root_name(n):
    while n.kind in ("member", "index"):
        n = n.obj
    return n.name if n.kind == "ident" else None


class Emitter:
    def __init__(self, structs):
        self.struct_names = structs
        self.structs = {}  # name -> (wname, {field: type})
        self.funcs = {}  # name -> [Func]
        self.scopes = [{}]
        self.helpers = {}
        self.pre = []
        self.out = []
        self.ind = 0
        self.uses_frag_coord = False
        self.late_inits = []
        self.assigned = set()
        self.main_out = None
        self.temps = set()
        self.ret = None

    # --- plumbing

    def line(self, text):
        self.out.append("    " * self.ind + text)

    def take_pre(self):
        pre, self.pre = self.pre, []
        return pre

    def flush_pre(self):
        for p in self.take_pre():
            self.line(p)

    def capture(self, fn):
        out, ind = self.out, self.ind
        self.out, self.ind = [], 0
        fn()
        lines = self.out
        self.out, self.ind = out, ind
        return lines

    def push(self):
        self.scopes.append({})

    def pop(self):
        self.scopes.pop()

    def declare(self, name, sym):
        self.scopes[-1][name] = sym

    def lookup(self, name):
        for s in reversed(self.scopes):
            if name in s:
                return s[name]
        return None

    def temp(self, base):
        name, k = base, 1
        while name in self.temps:
            name, k = f"{base}{k}", k + 1
        self.temps.add(name)
        return name

    def wtype(self, t):
        if isinstance(t, Arr):
            return f"array<{self.wtype(t.elem)}, {t.size}>"
        if t in SCALARS:
            return SCALARS[t]
        m = VEC_RE.match(t)
        if m:
            b = PREFIX_BASE[m.group(1)]
            return f"vec{m.group(2)}<bool>" if b == "bool" else f"vec{m.group(2)}{BASE_SUFFIX[b]}"
        if is_mat(t):
            return "mat{}x{}f".format(*mat_dims(t))
        if t in self.structs:
            return self.structs[t][0]
        raise ConvertError(0, f"type {t} has no WGSL equivalent")

    @staticmethod
    def primary(r):
        return r.code if r.prec >= PRIMARY else f"({r.code})"

    def convert(self, r, t):
        """r converted to type t where GLSL converts implicitly or by constructor."""
        if r.abstract and r.ty == "int" and base(t) == "float" and re.fullmatch(r"-?\d+", r.code):
            return R(r.code + ".0", "float", r.prec, r.op, True)
        if r.abstract or r.ty == t or isinstance(t, Arr) or t in self.structs:
            return r
        if base(r.ty) and base(t) and base(r.ty) != base(t) and ncomp(r.ty) == ncomp(t):
            return R(f"{self.wtype(t)}({r.code})", t)
        return r

    @staticmethod
    def same_type(r, t):
        if r.abstract:
            return r.ty == t and t in ("float", "int")
        return r.ty == t

    def sem_type(self, ty, arr, init_r):
        if arr is None:
            return ty
        if arr == AUTO:
            if init_r is None or not isinstance(init_r.ty, Arr):
                raise ConvertError(0, "unsized array needs an array initializer")
            return init_r.ty
        return Arr(ty, self.emit_expr(arr).code)

    # --- expressions

    def emit_expr(self, n):
        return getattr(self, "e_" + n.kind)(n)

    def e_num(self, n):
        s = n.text
        if s[:2].lower() == "0x":
            return R(s, "uint", abstract=False) if s[-1] in "uU" else R(s, "int", abstract=True)
        m = re.fullmatch(r"(\d*)(?:\.(\d*))?([eE][+-]?\d+)?([fFuU]?)", s)
        ip, fp, ex, suf = m.group(1), m.group(2), m.group(3) or "", m.group(4)
        if suf in ("u", "U"):
            return R(f"{int(ip)}u", "uint")
        if fp is not None or ex or suf:
            ip = ip.lstrip("0") or "0"
            frac = (fp or "").rstrip("0") or "0" if fp is not None or not ex else None
            code = f"{ip}.{frac}{ex}" if frac is not None else f"{ip}{ex}"
            return R(code, "float", abstract=True)
        val = int(ip, 8) if len(ip) > 1 and ip[0] == "0" else int(ip)
        return R(str(val), "int", abstract=True)

    def e_ident(self, n):
        name = n.name
        sym = self.lookup(name)
        if sym:
            if sym.kind == "ptr":
                return R("*" + sym.code, sym.ty, UNARY)
            return R(sym.code, sym.ty, abstract=sym.abstract)
        if name in ("true", "false"):
            return R(name, "bool")
        if name == "gl_FragCoord":
            self.uses_frag_coord = True
            return R("frag_coord", "vec4")
        if name in UNIFORMS:
            return R(*UNIFORMS[name])
        raise ConvertError(n.line, f"unknown identifier {name}")

    def e_member(self, n):
        sw = n.name.translate(str.maketrans("stpq", "xyzw"))
        if n.obj.kind == "ident" and not self.lookup(n.obj.name) and re.fullmatch(r"[xyzwrgba]+", sw):
            if n.obj.name == "iResolution" and re.fullmatch(r"[xyrg]+", sw):
                code = "ui.resolution" if sw in ("xy", "rg") else f"ui.resolution.{sw}"
                return R(code, vec("float", len(sw)))
            if n.obj.name == "iMouse":
                return R("0.0" if len(sw) == 1 else f"vec{len(sw)}f(0.0)", vec("float", len(sw)))
        o = self.emit_expr(n.obj)
        if o.ty in self.structs:
            fields = self.structs[o.ty][1]
            if n.name not in fields:
                raise ConvertError(n.line, f"{o.ty} has no field {n.name}")
            return R(f"{self.primary(o)}.{rename(n.name)}", fields[n.name])
        nc = ncomp(o.ty)
        if not nc or not re.fullmatch(r"[xyzw]+|[rgba]+", sw):
            raise ConvertError(n.line, f"can't take .{n.name} of {o.ty}")
        if nc == 1:
            return o if len(sw) == 1 else R(f"{self.wtype(vec(o.ty, len(sw)))}({o.code})", vec(o.ty, len(sw)))
        return R(f"{self.primary(o)}.{sw}", vec(base(o.ty), len(sw)))

    def e_index(self, n):
        if n.obj.kind == "ident" and n.obj.name == "iChannelResolution" and not self.lookup("iChannelResolution"):
            return R("vec3f(vec2f(textureDimensions(tex)), 1.0)", "vec3")
        o = self.emit_expr(n.obj)
        i = self.emit_expr(n.idx)
        if isinstance(o.ty, Arr):
            ty = o.ty.elem
        elif is_mat(o.ty):
            ty = vec("float", mat_dims(o.ty)[1])
        elif ncomp(o.ty) > 1:
            ty = base(o.ty)
        else:
            raise ConvertError(n.line, f"can't index {o.ty}")
        return R(f"{self.primary(o)}[{i.code}]", ty)

    def e_unary(self, n):
        if n.op in ("++", "--"):
            lv = self.emit_expr(n.e)
            self.pre.append(self.incdec(lv, n.op))
            return lv
        o = self.emit_expr(n.e)
        if n.op == "+":
            return o
        c = o.code if o.prec >= UNARY else f"({o.code})"
        if c[0] == n.op:
            c = f"({c})"
        return R(n.op + c, o.ty, UNARY, abstract=o.abstract)

    def e_postfix(self, n):
        lv = self.emit_expr(n.e)
        tmp = self.temp(f"{root_name(n.e) or 'v'}_old")
        self.pre += [f"let {tmp} = {lv.code};", self.incdec(lv, n.op)]
        return R(tmp, lv.ty)

    @staticmethod
    def incdec(lv, op):
        if base(lv.ty) in ("int", "uint"):
            return f"{lv.code}{op};"
        return f"{lv.code} {op[0]}= 1.0;"

    def operand(self, r, op, right):
        if r.prec >= UNARY:
            return r.code
        c, p = r.op, BINPREC[op]
        need = r.prec < p or (r.prec == p and right)
        if op in BITWISE:
            need = need or not (c == op and not right)
        elif op in SHIFT:
            need = True
        elif op in REL:
            need = need or c in REL
        elif op in LOGIC:
            need = need or c in BITWISE or (c in LOGIC and (c != op or right))
        return f"({r.code})" if need else r.code

    def binop(self, op, a, b):
        if op in SHIFT and base(b.ty) == "int" and not b.abstract:
            b = R(f"{self.wtype(vec('uint', ncomp(b.ty)))}({b.code})", vec("uint", ncomp(b.ty)))
        code = f"{self.operand(a, op, False)} {op} {self.operand(b, op, True)}"
        if op in ("==", "!=") and (ncomp(a.ty) > 1 or ncomp(b.ty) > 1):
            return R(f"{'all' if op == '==' else 'any'}({code})", "bool")
        ty, abstract = self.binop_type(op, a, b)
        return R(code, ty, BINPREC[op], op, abstract)

    @staticmethod
    def binop_type(op, a, b):
        if op in REL or op in LOGIC:
            return "bool", False
        ta, tb = a.ty, b.ty
        if op in SHIFT:
            return ta, a.abstract and b.abstract
        if op == "*" and is_mat(ta) and is_mat(tb):
            t = mat_name(mat_dims(tb)[0], mat_dims(ta)[1])
        elif op == "*" and is_mat(ta) and ncomp(tb) > 1:
            t = vec("float", mat_dims(ta)[1])
        elif op == "*" and ncomp(ta) > 1 and is_mat(tb):
            t = vec("float", mat_dims(tb)[0])
        elif is_mat(ta) or ncomp(ta) > 1:
            t = ta
        elif is_mat(tb) or ncomp(tb) > 1:
            t = tb
        elif a.abstract and not b.abstract:
            t = tb
        elif b.abstract and not a.abstract:
            t = ta
        else:
            t = "float" if "float" in (ta, tb) else ta
        return t, a.abstract and b.abstract

    def e_binary(self, n):
        a = self.emit_expr(n.a)
        b = self.emit_expr(n.b)
        return self.binop("!=" if n.op == "^^" else n.op, a, b)

    def e_ternary(self, n):
        c = self.emit_expr(n.cond)
        a = self.emit_expr(n.a)
        b = self.emit_expr(n.b)
        ty, abstract = self.binop_type("+", a, b)
        return R(f"select({b.code}, {a.code}, {c.code})", ty, abstract=abstract)

    def e_assign(self, n):
        self.pre += self.assign_stmts(n)
        return self.emit_expr(n.target)

    def e_comma(self, n):
        self.pre += self.expr_stmt(n.a)
        return self.emit_expr(n.b)

    def assign_stmts(self, n):
        t = n.target
        v = self.emit_expr(n.value)
        op = n.op
        if t.kind == "member" and len(t.name) > 1:
            o = self.emit_expr(t.obj)
            if ncomp(o.ty) > 1:
                oc = self.primary(o)
                sw = t.name.translate(str.maketrans("stpqrgba", "xyzwxyzw"))
                val = v.code if op == "=" else f"{oc}.{sw} {op[:-1]} {self.operand(v, op[:-1], True)}"
                tmp = self.temp(f"{root_name(t) or 'v'}_{sw}")
                return [f"let {tmp} = {val};"] + [f"{oc}.{c} = {tmp}.{'xyzw'[i]};" for i, c in enumerate(sw)]
        lv = self.emit_expr(t)
        if op in ("<<=", ">>=") and base(v.ty) == "int" and not v.abstract:
            v = R(f"{self.wtype(vec('uint', ncomp(v.ty)))}({v.code})", vec("uint", ncomp(v.ty)))
        if op == "=":
            v = self.convert(v, lv.ty)
        return [f"{lv.code} {op} {v.code};"]

    def expr_stmt(self, n):
        """Statement lines for an expression evaluated for its side effects."""
        outer = self.take_pre()
        if n.kind == "assign":
            lines = self.assign_stmts(n)
        elif n.kind == "postfix" or n.kind == "unary" and n.op in ("++", "--"):
            lines = [self.incdec(self.emit_expr(n.e), n.op)]
        elif n.kind == "comma":
            lines = self.expr_stmt(n.a) + self.expr_stmt(n.b)
        else:
            r = self.emit_expr(n)
            lines = [f"{r.code};" if r.ty == "void" else f"_ = {r.code};"]
        lines = self.pre + lines
        self.pre = outer
        return lines

    def e_call(self, n):
        if n.ctor:
            return self.emit_ctor(n)
        if n.name in self.funcs:
            return self.emit_user_call(n)
        if n.name in TEXTURE_FNS:
            return self.emit_texture(n)
        return self.emit_builtin(n)

    def emit_ctor(self, n):
        t = n.name
        args = [self.emit_expr(a) for a in n.args]
        if n.arr is not None:
            size = str(len(args)) if n.arr == AUTO else self.emit_expr(n.arr).code
            codes = [self.convert(a, t).code for a in args]
            return R(f"array<{self.wtype(t)}, {size}>({', '.join(codes)})", Arr(t, size))
        if t in self.structs:
            fields = list(self.structs[t][1].values())
            codes = [self.convert(a, ft).code for a, ft in zip(args, fields)]
            return R(f"{self.structs[t][0]}({', '.join(codes)})", t)
        if t in SCALARS:
            if len(args) != 1:
                raise ConvertError(n.line, f"{t}() takes one argument")
            a = args[0]
            if ncomp(a.ty) > 1 or is_mat(a.ty):
                a = R(f"{self.primary(a)}.x" if ncomp(a.ty) else f"{self.primary(a)}[0].x", base(a.ty))
            if a.ty == t and not a.abstract:
                return a
            return R(f"{SCALARS[t]}({a.code})", t)
        if ncomp(t) > 1:
            n_ = ncomp(t)
            if len(args) == 1 and ncomp(args[0].ty) > n_:
                a = args[0]
                args = [R(f"{self.primary(a)}.{'xyzw'[:n_]}", vec(base(a.ty), n_))]
            if len(args) == 1 and ncomp(args[0].ty) == n_:
                return self.convert(args[0], t) if args[0].ty != t else args[0]
            codes = [self.convert(a, vec(base(t), ncomp(a.ty) or 1)).code for a in args]
            return R(f"{self.wtype(t)}({', '.join(codes)})", t)
        if is_mat(t):
            c, r = mat_dims(t)
            if len(args) == 1 and ncomp(args[0].ty) == 1:
                d = self.convert(args[0], "float").code
                codes = [d if i == j else "0.0" for i in range(c) for j in range(r)]
            elif len(args) == 1 and is_mat(args[0].ty):
                ac, ar = mat_dims(args[0].ty)
                if ac < c or ar < r:
                    raise ConvertError(n.line, f"can't build {t} from {args[0].ty}")
                m = self.primary(args[0])
                codes = [f"{m}[{i}].{'xyzw'[:r]}" if ar > r else f"{m}[{i}]" for i in range(c)]
            else:
                codes = [self.convert(a, vec("float", ncomp(a.ty) or 1)).code for a in args]
            return R(f"{self.wtype(t)}({', '.join(codes)})", t)
        raise ConvertError(n.line, f"can't construct {t}")

    def resolve(self, n, args):
        cands = [f for f in self.funcs[n.name] if len(f.params) == len(args)]
        if not cands:
            raise ConvertError(n.line, f"no {n.name} takes {len(args)} arguments")
        for f in cands:
            if all(
                a.ty == pt or (a.abstract and ncomp(pt) == 1 and pt != "bool")
                for (_, pt, _), a in zip(f.params, args)
            ):
                return f
        return cands[0]

    def emit_user_call(self, n):
        args = [self.emit_expr(a) for a in n.args]
        f = self.resolve(n, args)
        codes = []
        for (kind, pt, _), a, node in zip(f.params, args, n.args):
            if kind == "sampler":
                continue
            if kind in ("out", "inout"):
                if node.kind == "member" and ncomp(self.emit_expr(node.obj).ty) > 1:
                    raise ConvertError(n.line, "a swizzle can't be an out argument in WGSL")
                codes.append(a.code[1:] if a.code.startswith("*") and a.prec == UNARY else "&" + a.code)
            else:
                codes.append(self.convert(a, pt).code)
        return R(f"{f.wname}({', '.join(codes)})", f.ret)

    def emit_texture(self, n):
        s = self.emit_expr(n.args[0])
        if s.ty != "sampler2D":
            raise ConvertError(n.line, f"{n.name} needs an iChannel")
        args = [self.emit_expr(a) for a in n.args[1:]]
        if n.name == "textureSize":
            lod = args[0].code if args else "0"
            return R(f"vec2i(textureDimensions(tex, {lod}))", "ivec2")
        uv = args[0]
        if n.name == "texelFetch":
            p = self.convert(uv, "ivec2").code
            return R(f"textureLoad(tex, {p}, {args[1].code})", "vec4")
        if ncomp(uv.ty) != 2:
            raise ConvertError(n.line, "only 2D textures are supported")
        if n.name in ("texture", "texture2D"):
            return R(f"textureSampleLevel(tex, samp, {uv.code}, 0.0)", "vec4")
        if n.name in ("textureLod", "texture2DLod"):
            lod = self.convert(args[1], "float").code
            return R(f"textureSampleLevel(tex, samp, {uv.code}, {lod})", "vec4")
        return R(f"textureSampleGrad(tex, samp, {uv.code}, {args[1].code}, {args[2].code})", "vec4")

    def emit_builtin(self, n):
        name = n.name
        args = [self.emit_expr(a) for a in n.args]
        abstract = all(a.abstract for a in args)
        if name in COMPARE:
            op = COMPARE[name]
            code = f"{self.operand(args[0], op, False)} {op} {self.operand(args[1], op, True)}"
            return R(code, vec("bool", ncomp(args[0].ty)), BINPREC[op], op)
        if name == "not":
            return R(f"!{self.primary(args[0])}", args[0].ty, UNARY)
        if name in ("any", "all"):
            return R(f"{name}({args[0].code})", "bool")
        if name in BITCASTS:
            t = vec(BITCASTS[name], ncomp(args[0].ty))
            return R(f"bitcast<{self.wtype(t)}>({args[0].code})", t)
        if name == "isnan":
            a = self.primary(args[0])
            return self.binop("!=", R(a, args[0].ty), R(a, args[0].ty))
        if name == "mix" and len(args) == 3 and base(args[2].ty) == "bool":
            return R(f"select({args[0].code}, {args[1].code}, {args[2].code})", args[0].ty)
        if name in ("length", "distance", "dot", "determinant"):
            return R(f"{name}({', '.join(a.code for a in args)})", "float", abstract=abstract)
        if name == "cross":
            return R(f"cross({args[0].code}, {args[1].code})", "vec3")
        if name in SAME_TYPE or (name == "atan" and len(args) == 1):
            ty = "float" if args[0].ty == "int" and args[0].abstract else args[0].ty
            if is_mat(ty) and name == "transpose":
                c, r = mat_dims(ty)
                ty = mat_name(r, c)
            codes = ", ".join(a.code for a in args)
            return R(f"{RENAMES.get(name, name)}({codes})", ty, abstract=abstract)
        if name in SPLAT or name == "mix":
            return self.emit_splat(n, args, abstract)
        raise ConvertError(n.line, f"unsupported function {name}")

    def emit_splat(self, n, args, abstract):
        name = n.name
        vargs = args[:2] if name == "mix" else args
        target = next((a.ty for a in vargs if ncomp(a.ty) > 1), None)
        if target is None:
            target = next((a.ty for a in args if not a.abstract), "float")
            if target == "int" and name not in ("min", "max", "clamp"):
                target = "float"
        codes = []
        for i, a in enumerate(args):
            if ncomp(a.ty) == 1 and ncomp(target) > 1 and not (name == "mix" and i == 2):
                a = self.convert(a, base(target))
                codes.append(f"{self.wtype(target)}({a.code})")
            else:
                codes.append(self.convert(a, target if i < 2 or name != "mix" else a.ty).code)
        if name == "mod":
            fn = "mod_" + self.wtype(target).replace("<", "").replace(">", "")
            wt = self.wtype(target)
            self.helpers[fn] = (
                f"fn {fn}(x: {wt}, y: {wt}) -> {wt} {{\n    return x - y * floor(x / y);\n}}"
            )
            return R(f"{fn}({', '.join(codes)})", target)
        fname = "atan2" if name == "atan" else name
        return R(f"{fname}({', '.join(codes)})", target, abstract=abstract)

    # --- statements

    def emit_body(self, s):
        self.push()
        self.ind += 1
        for st in s.stmts if s.kind == "block" else [s]:
            self.emit_stmt(st)
        self.ind -= 1
        self.pop()

    def emit_stmt(self, s):
        getattr(self, "s_" + s.kind)(s)

    def s_empty(self, s):
        pass

    def s_block(self, s):
        self.line("{")
        self.emit_body(s)
        self.line("}")

    def s_expr(self, s):
        for line in self.expr_stmt(s.e):
            self.line(line)

    def s_decl(self, s):
        for name, arr, init in s.items:
            r = self.emit_expr(init) if init is not None else None
            try:
                ty = self.sem_type(s.ty, arr, r)
            except ConvertError as e:
                raise ConvertError(s.line, str(e).split(": ", 1)[-1])
            self.flush_pre()
            wn = rename(name)
            if r is None:
                self.line(f"var {wn}: {self.wtype(ty)};")
            else:
                r = self.convert(r, ty)
                kw = "var" if name in self.assigned or isinstance(ty, Arr) else "let"
                annot = "" if self.same_type(r, ty) else f": {self.wtype(ty)}"
                self.line(f"{kw} {wn}{annot} = {r.code};")
            self.declare(name, Sym(wn, ty, "var"))

    def s_if(self, s):
        c = self.emit_expr(s.cond)
        self.flush_pre()
        self.line(f"if {c.code} {{")
        self.emit_body(s.then)
        e, closes = s.els, 0
        while e is not None and e.kind == "if":
            c = self.emit_expr(e.cond)
            pre = self.take_pre()
            if pre:
                self.line("} else {")
                self.ind += 1
                closes += 1
                for p in pre:
                    self.line(p)
                self.line(f"if {c.code} {{")
            else:
                self.line(f"}} else if {c.code} {{")
            self.emit_body(e.then)
            e = e.els
        if e is not None:
            self.line("} else {")
            self.emit_body(e)
        self.line("}")
        for _ in range(closes):
            self.ind -= 1
            self.line("}")

    def emit_loop(self, init, cond, cond_pre, step, body):
        """A loop WGSL's for/while can't express, as loop/continuing."""
        if init:
            self.line("{")
            self.ind += 1
            for line in init:
                self.line(line)
        self.line("loop {")
        self.ind += 1
        for p in cond_pre:
            self.line(p)
        if cond is not None:
            self.line(f"if !{self.primary(cond)} {{ break; }}")
        self.push()
        for st in body.stmts if body.kind == "block" else [body]:
            self.emit_stmt(st)
        self.pop()
        if step:
            self.line("continuing {")
            for line in step:
                self.line("    " + line)
            self.line("}")
        self.ind -= 1
        self.line("}")
        if init:
            self.ind -= 1
            self.line("}")

    def s_for(self, s):
        self.push()
        init = self.capture(lambda: s.init and self.emit_stmt(s.init))
        cond = self.emit_expr(s.cond) if s.cond else None
        cond_pre = self.take_pre()
        step = self.expr_stmt(s.step) if s.step else []
        if len(init) <= 1 and not cond_pre and len(step) <= 1:
            i = init[0].rstrip(";") if init else ""
            st = step[0].rstrip(";") if step else ""
            self.line(f"for ({i}; {cond.code if cond else ''}; {st}) {{")
            self.emit_body(s.body)
            self.line("}")
        else:
            self.emit_loop(init, cond, cond_pre, step, s.body)
        self.pop()

    def s_while(self, s):
        cond = self.emit_expr(s.cond)
        pre = self.take_pre()
        if pre:
            self.emit_loop([], cond, pre, [], s.body)
            return
        self.line(f"while {cond.code} {{")
        self.emit_body(s.body)
        self.line("}")

    def s_dowhile(self, s):
        self.line("loop {")
        self.emit_body(s.body)
        self.ind += 1
        self.line("continuing {")
        self.ind += 1
        cond = self.emit_expr(s.cond)
        self.flush_pre()
        self.line(f"break if !{self.primary(cond)};")
        self.ind -= 1
        self.line("}")
        self.ind -= 1
        self.line("}")

    def s_return(self, s):
        if self.main_out:
            if s.e is not None:
                raise ConvertError(s.line, "mainImage returns a value")
            self.line(f"return {self.main_out};")
            return
        if s.e is None:
            self.line("return;")
            return
        r = self.convert(self.emit_expr(s.e), self.ret)
        self.flush_pre()
        self.line(f"return {r.code};")

    def s_break(self, s):
        self.line("break;")

    def s_continue(self, s):
        self.line("continue;")

    def s_discard(self, s):
        self.line("discard;")

    def s_switch(self, s):
        v = self.emit_expr(s.e)
        self.flush_pre()
        self.line(f"switch {v.code} {{")
        self.ind += 1
        for i, (labels, stmts) in enumerate(s.cases):
            if stmts and stmts[-1].kind == "break":
                stmts = stmts[:-1]
            elif i + 1 < len(s.cases) and not (stmts and stmts[-1].kind in ("return", "continue", "discard")):
                raise ConvertError(s.line, "switch fallthrough is not supported in WGSL")
            labs = ", ".join("default" if lab == "default" else self.emit_expr(lab).code for lab in labels)
            self.line(f"{'default' if labs == 'default' else 'case ' + labs}: {{")
            self.emit_body(Node("block", s.line, stmts=stmts))
            self.line("}")
        self.ind -= 1
        self.line("}")

    # --- top level

    def collect_assigned(self, body):
        names = set()
        for n in walk(body):
            if n.kind == "assign":
                names.add(root_name(n.target))
            elif n.kind in ("unary", "postfix") and n.op in ("++", "--"):
                names.add(root_name(n.e))
            elif n.kind == "call" and n.name in self.funcs:
                for f in self.funcs[n.name]:
                    for (kind, _, _), a in zip(f.params, n.args):
                        if kind in ("out", "inout"):
                            names.add(root_name(a))
        return names

    def is_const_expr(self, n):
        for e in walk(n):
            if e.kind == "ident":
                sym = self.lookup(e.name)
                if not (sym and sym.kind == "const" or e.name in ("true", "false")):
                    return False
            elif e.kind == "call" and not e.ctor or e.kind in ("index", "assign", "ternary", "postfix"):
                return False
        return True

    def emit_global(self, s):
        for name, arr, init in s.items:
            r = self.emit_expr(init) if init is not None else None
            if self.pre:
                raise ConvertError(s.line, "side effects in a global initializer")
            ty = self.sem_type(s.ty, arr, r)
            wn = rename(name)
            kind = "global"
            if r is None:
                self.line(f"var<private> {wn}: {self.wtype(ty)};")
            else:
                r = self.convert(r, ty)
                annot = "" if self.same_type(r, ty) else f": {self.wtype(ty)}"
                if not self.is_const_expr(init):
                    self.line(f"var<private> {wn}: {self.wtype(ty)};")
                    self.late_inits.append(f"{wn} = {r.code};")
                elif "const" in s.quals and not isinstance(ty, Arr):
                    self.line(f"const {wn}{annot} = {r.code};")
                    kind = "const"
                else:
                    self.line(f"var<private> {wn}{annot} = {r.code};")
            self.declare(name, Sym(wn, ty, kind))

    def emit_function(self, f):
        node = f.node
        self.assigned = self.collect_assigned(node.body)
        self.temps = set()
        self.ret = f.ret
        is_main = node.name == "mainImage"
        self.push()
        params, prologue = [], []
        for kind, pt, pname in f.params:
            wn = rename(pname)
            if kind == "sampler":
                self.declare(pname, Sym("tex", "sampler2D", "sampler"))
            elif is_main and kind == "out":
                prologue.append(f"var {wn} = vec4f(0.0);")
                self.declare(pname, Sym(wn, pt, "var"))
                self.main_out = wn
            elif kind in ("out", "inout"):
                params.append(f"{wn}: ptr<function, {self.wtype(pt)}>")
                self.declare(pname, Sym(wn, pt, "ptr"))
            elif pname in self.assigned:
                params.append(f"{wn}_in: {self.wtype(pt)}")
                prologue.append(f"var {wn} = {wn}_in;")
                self.declare(pname, Sym(wn, pt, "var"))
            else:
                params.append(f"{wn}: {self.wtype(pt)}")
                self.declare(pname, Sym(wn, pt, "let"))
        ret = "" if f.ret == "void" else f" -> {self.wtype(f.ret)}"
        self.line(f"fn {f.wname}({', '.join(params)}){ret} {{")
        self.ind += 1
        for p in prologue:
            self.line(p)
        for st in node.body.stmts:
            self.emit_stmt(st)
        if is_main and not (node.body.stmts and node.body.stmts[-1].kind == "return"):
            self.line(f"return {self.main_out};")
        self.ind -= 1
        self.line("}")
        self.pop()
        self.main_out = None

    def register(self, items):
        for it in items:
            if it.kind == "struct":
                fields = {}
                for ty, fname, arr in it.fields:
                    fields[fname] = ty if arr is None else Arr(ty, self.emit_expr(arr).code)
                self.structs[it.name] = (rename(it.name), fields)
            elif it.kind == "func" and it.body is not None:
                params = []
                for quals, ty, pname in it.params:
                    if ty == "sampler2D":
                        kind = "sampler"
                    else:
                        kind = next((q for q in ("inout", "out") if q in quals), "in")
                    params.append((kind, ty, pname))
                self.funcs.setdefault(it.name, []).append(Func(it, rename(it.name), params, it.ret))
        for overloads in self.funcs.values():
            if len(overloads) > 1:
                for f in overloads:
                    sig = "_".join(re.sub(r"\W", "", self.wtype(t)) for k, t, _ in f.params if k != "sampler")
                    f.wname = f"{f.wname}_{sig}"
        mains = self.funcs.get("mainImage")
        if not mains:
            raise ConvertError(0, "no mainImage function")
        m = mains[0]
        if [(k, t) for k, t, _ in m.params] != [("out", "vec4"), ("in", "vec2")]:
            raise ConvertError(m.node.line, "mainImage must be (out vec4, vec2)")
        m.ret = "vec4"


def convert(src):
    pp = Preprocessor()
    toks = pp.run(src)
    parser = Parser(toks)
    items = parser.parse_unit()
    em = Emitter(parser.structs)
    em.register(items)

    sections = []
    used = {t.val for t in toks if t.kind == "id"}
    for body in pp.consts.values():
        used |= {t.val for t in body if t.kind == "id"}
    consts = []
    for name, body in pp.consts.items():
        if name not in used:
            continue
        r = em.emit_expr(Parser(body).parse_expr())
        wn = rename(name)
        consts.append(f"const {wn} = {r.code};")
        em.declare(name, Sym(wn, r.ty, "const", r.abstract))
    if consts:
        sections.append("\n".join(consts))

    for it in items:
        if it.kind == "struct":
            wname, fields = em.structs[it.name]
            body = "".join(f"    {rename(f)}: {em.wtype(t)},\n" for f, t in fields.items())
            sections.append(f"struct {wname} {{\n{body}}}")

    em.out = []
    for it in items:
        if it.kind == "decl":
            em.emit_global(it)
    if em.out:
        sections.append("\n".join(em.out))

    funcs = []
    for it in items:
        if it.kind == "func" and it.body is not None:
            em.out = []
            f = next(f for f in em.funcs[it.name] if f.node is it)
            try:
                em.emit_function(f)
            except ConvertError as e:
                if str(e).startswith("0: "):
                    raise ConvertError(it.line, str(e)[3:])
                raise
            funcs.append("\n".join(em.out))

    if em.uses_frag_coord:
        sections.append("var<private> frag_coord: vec4f;")
    sections += em.helpers.values()
    sections += funcs

    entry = [
        "@fragment",
        "fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4f {",
        "    let coord = vec2f(in.uv.x, 1.0 - in.uv.y) * ui.resolution;",
    ]
    if em.uses_frag_coord:
        entry.append("    frag_coord = vec4f(coord, 0.0, 1.0);")
    entry += ["    " + line for line in em.late_inits]
    entry += ["    return vec4f(mainImage(coord).rgb, 1.0);", "}"]
    sections.append("\n".join(entry))
    return "\n\n".join(sections) + "\n"


def main():
    args = sys.argv[1:]
    if len(args) not in (1, 2):
        print(__doc__.strip(), file=sys.stderr)
        return 1
    with open(args[0]) as f:
        src = f.read()
    try:
        wgsl = convert(src)
    except ConvertError as e:
        print(f"{args[0]}:{e}", file=sys.stderr)
        return 1
    if len(args) == 2:
        with open(args[1], "w") as f:
            f.write(wgsl)
    else:
        sys.stdout.write(wgsl)
    return 0


if __name__ == "__main__":
    sys.exit(main())
