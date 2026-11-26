#!/usr/bin/env python3
"""Demarc logo: C64-style keycap D + rainbow cursor stripes.

Writes demarc-logo/art/{icon,logo,logo-alt}.svg and the Android launcher
foreground PNGs (needs rsvg-convert)."""

import os
import subprocess

from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.ttLib import TTFont

ROOT = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(ROOT, "art")
ANDROID_RES = os.path.join(ROOT, "../android/app/src/main/res")
FONT = "C64Keyboard-Regular.otf"

BONE = "#E4DFD3"
BG = "#15171B"
BG_FRAME = "#2C3036"
RAINBOW = ["#C8463A", "#D9822B", "#DDB840", "#5E9A4A", "#3D6FB0"]
KEY_SKIRT = "#33291F"
KEY_CREASE = "#2A2219"
KEY_TOP = "#5A4A3E"
KEY_DISH = "#52433A"

LEGEND_SIZE = 220  # cap height of the D on the keycap, key-local units (key is 440)
TEXT_SIZE = 176  # cap height of "EMARC" next to the keycap, px
KEY_TEXT_GAP = 3  # px
WM_KEY_SCALE = 0.63
LETTER_GAP = 0.12  # fraction of cap height
WIDE_TEXT_SIZE = 140  # cap height of "DEMARC" on the wide key, px
WIDE_KEY_PAD = 50  # dish edge to logo, key-local units
DISH_CY = 202  # key-local units
CURSOR_UNIT = 1 / 6.9  # rainbow cursor unit, fraction of cap height

ICON, WM_W, WM_H = 1024, 1600, 520
ICON_KEY_SCALE = (
    0.88  # keycap diagonal stays inside the 626px adaptive-icon safe circle
)
ICON_RAINBOW_BG = True  # fill the icon background with rainbow stripes
MARGIN_Y = (
    18 / 108
) * 0.9  # added to the outer rainbow bars; Android masks this much per edge
ANDROID_SIZES = {"mdpi": 108, "hdpi": 162, "xhdpi": 216, "xxhdpi": 324, "xxxhdpi": 432}

_font = TTFont(os.path.join(ROOT, FONT))
_gs, _cmap = _font.getGlyphSet(), _font.getBestCmap()
_cap = getattr(_font["OS/2"], "sCapHeight", 0) or _font["head"].unitsPerEm * 0.7


def fmt(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


def svg(w, h, body):
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
        f'viewBox="0 0 {w} {h}">\n{body}\n</svg>\n'
    )


def background(w, h):
    m, r = min(w, h) * 0.06, min(w, h) * 0.05
    return (
        f'<rect width="{w}" height="{h}" fill="{BG}"/>'
        f'<rect x="{fmt(m)}" y="{fmt(m)}" width="{fmt(w - 2 * m)}" height="{fmt(h - 2 * m)}" '
        f'rx="{fmt(r)}" fill="none" stroke="{BG_FRAME}" stroke-width="{fmt(min(w, h) * 0.006)}"/>'
    )


def glyph(ch, size, x, y):
    """Glyph with cap height `size` and ink box top-left at (x, y).
    Returns (svg, ink width)."""
    g = _gs[_cmap[ord(ch)]]
    bp = BoundsPen(_gs)
    g.draw(bp)
    x0, _, x1, _ = bp.bounds
    pp = SVGPathPen(_gs)
    g.draw(pp)
    k = size / _cap
    return (
        f'<path transform="translate({fmt(x - x0 * k)} {fmt(y + size)}) '
        f'scale({k:.5f} {-k:.5f})" d="{pp.getCommands()}" fill="{BONE}"/>'
    ), (x1 - x0) * k


def legend_top():
    return DISH_CY - LEGEND_SIZE / 2


def keycap(s, w=440, legend=True):
    """Keycap seen from above, w x 440 key-local units scaled by s.
    Returns (svg, width, height)."""
    r = 22
    ex = w - 440
    skirt = (0, 0, w, 440, 56)
    top = (46, 30, 348 + ex, 344, 30)
    dish = (62, 46, 316 + ex, 312, 22)

    def rr(b, fill):
        return (
            f'<rect x="{fmt(b[0])}" y="{fmt(b[1])}" width="{fmt(b[2])}" height="{fmt(b[3])}" '
            f'rx="{b[4]}" fill="{fill}"/>'
        )

    ox0, oy0, ox1, oy1 = 22, 22, w - 22, 418
    tx0, ty0 = top[0] + 9, top[1] + 9
    tx1, ty1 = top[0] + top[2] - 9, top[1] + top[3] - 9
    creases = "".join(
        f'<path d="M{fmt(ax)} {ay} L{fmt(bx)} {by}" stroke="{KEY_CREASE}" '
        f'stroke-width="4" stroke-linecap="round"/>'
        for ax, ay, bx, by in (
            (ox0, oy0, tx0, ty0),
            (ox1, oy0, tx1, ty0),
            (ox0, oy1, tx0, ty1),
            (ox1, oy1, tx1, ty1),
        )
    )
    body = [rr(skirt, KEY_SKIRT), creases, rr(top, KEY_TOP), rr(dish, KEY_DISH)]
    if legend:
        _, gw = glyph("D", LEGEND_SIZE, 0, 0)
        body.append(glyph("D", LEGEND_SIZE, 220 - gw / 2, legend_top())[0])
    return (
        f'<g transform="scale({fmt(s)}) translate({r} {r})">' + "".join(body) + "</g>",
        (w + 2 * r) * s,
        (440 + 2 * r) * s,
    )


def rainbow(x, y, w, h, margin=0.0):
    """Stripes split the band inside `margin` (fraction of h); the first and
    last stripe also cover the margins."""
    n, m = len(RAINBOW), margin * h
    edges = [y + m + i * (h - 2 * m) / n for i in range(n + 1)]
    edges[0], edges[-1] = y, y + h
    return "".join(
        f'<rect x="{fmt(x)}" y="{fmt(edges[i])}" width="{fmt(w)}" '
        f'height="{fmt(edges[i + 1] - edges[i] + 0.5)}" fill="{c}"/>'
        for i, c in enumerate(RAINBOW)
    )


def wordmark(letters, size, x0, y0):
    """Letters plus rainbow cursor. Returns (svg, right edge)."""
    parts, x = [], x0
    for ch in letters:
        p, w = glyph(ch, size, x, y0)
        parts.append(p)
        x += w + LETTER_GAP * size
    u = size * CURSOR_UNIT
    x = x - LETTER_GAP * size + u
    parts.append(rainbow(x, y0, 3 * u, size))
    return "".join(parts), x + 3 * u


def at(x, y, body):
    return f'<g transform="translate({fmt(x)} {fmt(y)})">{body}</g>'


def icon_fg():
    kb, kw, kh = keycap(ICON_KEY_SCALE)
    return at((ICON - kw) / 2, (ICON - kh) / 2 + 10, kb)


def keycap_logo():
    """Keycap D followed by "EMARC", centred on the keycap vertically."""
    kb, kw, kh = keycap(WM_KEY_SCALE)
    _, w_letters = wordmark("EMARC", TEXT_SIZE, 0, 0)
    x0 = (WM_W - (kw + KEY_TEXT_GAP + w_letters)) / 2
    ky = (WM_H - kh) / 2
    letters, _ = wordmark(
        "EMARC", TEXT_SIZE, x0 + kw + KEY_TEXT_GAP, (WM_H - TEXT_SIZE) / 2
    )
    return at(x0, ky, kb) + letters


def widekey_logo():
    """ "DEMARC" centred in the dish of one wide key."""
    s = WM_KEY_SCALE
    _, w_logo = wordmark("DEMARC", WIDE_TEXT_SIZE, 0, 0)
    kw_local = w_logo / s + 2 * (62 + WIDE_KEY_PAD)
    kb, kw, kh = keycap(s, kw_local, legend=False)
    x0, ky = (WM_W - kw) / 2, (WM_H - kh) / 2
    cx, cy = x0 + (22 + kw_local / 2) * s, ky + (22 + DISH_CY) * s
    logo, _ = wordmark(
        "DEMARC", WIDE_TEXT_SIZE, cx - w_logo / 2, cy - WIDE_TEXT_SIZE / 2
    )
    return at(x0, ky, kb) + logo


def icon_bg():
    if ICON_RAINBOW_BG:
        return rainbow(0, 0, ICON, ICON, MARGIN_Y)
    return background(ICON, ICON)


def android_icons():
    """Adaptive-icon layers: foreground PNGs, and a background that is either
    rainbow PNGs or @color/ic_launcher_background."""
    layers = {"foreground": icon_fg()}
    if ICON_RAINBOW_BG:
        layers["background"] = icon_bg()
    for dpi, px in ANDROID_SIZES.items():
        for name in ("foreground", "background"):
            out = os.path.join(ANDROID_RES, f"mipmap-{dpi}", f"ic_launcher_{name}.png")
            if name not in layers:
                if os.path.exists(out):
                    os.remove(out)
                continue
            subprocess.run(
                ["rsvg-convert", "-w", str(px), "-h", str(px), "-o", out],
                input=svg(ICON, ICON, layers[name]).encode(),
                check=True,
            )
    bg = (
        "@mipmap/ic_launcher_background"
        if ICON_RAINBOW_BG
        else "@color/ic_launcher_background"
    )
    with open(
        os.path.join(ANDROID_RES, "mipmap-anydpi-v26", "ic_launcher.xml"), "w"
    ) as f:
        f.write(
            '<?xml version="1.0" encoding="utf-8"?>\n'
            '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
            f'    <background android:drawable="{bg}" />\n'
            '    <foreground android:drawable="@mipmap/ic_launcher_foreground" />\n'
            "</adaptive-icon>\n"
        )
    with open(os.path.join(ANDROID_RES, "values", "colors.xml"), "w") as f:
        f.write(
            '<?xml version="1.0" encoding="utf-8"?>\n<resources>\n'
            f'    <color name="ic_launcher_background">{BG}</color>\n</resources>\n'
        )


os.makedirs(OUT, exist_ok=True)
files = {
    "icon.svg": svg(ICON, ICON, icon_bg() + icon_fg()),
    "logo.svg": svg(WM_W, WM_H, background(WM_W, WM_H) + widekey_logo()),
    "logo-alt.svg": svg(WM_W, WM_H, background(WM_W, WM_H) + keycap_logo()),
}
for fn, s in files.items():
    with open(os.path.join(OUT, fn), "w") as f:
        f.write(s)
android_icons()
