# wingding.ttf

A Wingdings stand-in: a symbol-encoded font (`cmap` 3,0 at `U+F020..F0FE`)
named `Wingdings`, with each glyph taken from the Unicode character Unicode 7.0
maps that Wingdings code to. Wine's own `wingding.ttf` has 49 glyphs, so text
like ReelTime's (`zoo.379064`) thumbs up/down (`C`, `D`) came out as boxes.
The font revision is 9.0 so wine prefers it over its builtin one.

Built from Noto fonts (SIL Open Font License 1.1) and shipped as
`system/win/prefix/wingding.ttf`:

```sh
N=/usr/share/fonts/noto
python3 build.py ../../system/win/prefix/wingding.ttf $N/NotoSansSymbols2-Regular.ttf \
    $N/NotoSansSymbols-Regular.ttf $N/NotoSansMath-Regular.ttf $N/NotoSans-Regular.ttf \
    $N/NotoSansDevanagari-Regular.ttf
```

Only `0x53` (droplet, U+1F4A7) is missing; no source above has it.
