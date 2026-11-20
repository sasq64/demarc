#!/usr/bin/env python3
"""Cut the csdb and demozoo dbs down to what minimarc shows, for the APK.

    android_dbs.py csdb IN OUT
    android_dbs.py demozoo-amiga IN OUT

csdb's download urls are shortened to the one-letter link classes of `LINK_BASES` in
crates/release-io/src/fetch.rs; `LINKS` here must match it.
"""
import re
import sys
from pathlib import Path

LINKS = {
    "A": "http://csdb.dk/getinternalfile.php",
    "B": "https://csdb.dk/getinternalfile.php",
    "C": "ftp://ftp.scs-trc.net/pub/c64",
    "D": "ftp://ftp.padua.org/pub/c64",
    "E": "ftp://c64.rulez.org/pub/c64",
}

AMIGA_PLATFORMS = {"Amiga", "Amiga AGA"}
AMIGA_CATEGORY = re.compile(
    r"Demo|Intro|\d+[kK] Intro|Cracktro|Invitation|Musicdisk|Diskmag|Slideshow"
)

ROOT = Path(__file__).resolve().parent.parent


def csdb_categories():
    src = (ROOT / "crates/database/src/csdb_latest.rs").read_text()
    block = re.search(r"pub const CATEGORIES[^=]*= &\[(.*?)\];", src, re.S)[1]
    return set(re.findall(r'"([^"]+)"', block))


def shorten(url):
    for code, base in LINKS.items():
        if url.startswith(base + "/"):
            return f"{code}:{url[len(base):]}"
    return url


def shorten_line(line):
    fields = line.split("\t")
    for i, f in enumerate(fields):
        if f.startswith("download:"):
            urls = f[len("download:") :].split(";")
            fields[i] = "download:" + ";".join(map(shorten, urls))
    return "\t".join(fields)


def code_table(header):
    table = {}
    for entry in header.split(","):
        code, _, name = entry.partition(":")
        table[code] = name
    return table


def fields(line):
    return line.rstrip("\n").split("\t")


def csdb(lines):
    keep = csdb_categories()
    for line in lines:
        if line.startswith("#"):
            yield line
            continue
        fs = fields(line)
        if any(f.startswith("download:") for f in fs) and any(
            f.startswith("category:") and f[len("category:") :] in keep for f in fs
        ):
            yield line


def demozoo_amiga(lines):
    platforms, categories = {}, {}
    for line in lines:
        if line.startswith("#"):
            if line.startswith("# PLATFORMS "):
                platforms = code_table(line[len("# PLATFORMS ") :].rstrip("\n"))
            elif line.startswith("# CATEGORIES "):
                categories = code_table(line[len("# CATEGORIES ") :].rstrip("\n"))
            yield line
            continue
        if not platforms:
            continue
        fs = fields(line)
        plats = {platforms.get(c) for f in fs if re.fullmatch(r"p\w+", f) for c in f[1:]}
        cats = {categories.get(c) for f in fs if re.fullmatch(r"c\w+", f) for c in f[1:]}
        if plats & AMIGA_PLATFORMS and any(
            c and AMIGA_CATEGORY.fullmatch(c) for c in cats
        ):
            yield line


def main():
    kind, src, dst = sys.argv[1:]
    with open(src, encoding="utf-8") as f:
        lines = list(f)
    out = []
    if kind == "csdb":
        out = csdb(lines)
    elif kind == "demozoo-amiga":
        out = list(demozoo_amiga(lines))
        header = sum(1 for l in out if l.startswith("#"))
        out.insert(header, "# release_url:https://demozoo.org/productions/{id}/\n")
    else:
        sys.exit(f"unknown db {kind}")
    with open(dst, "w", encoding="utf-8") as f:
        for line in out:
            f.write(line if kind != "csdb" or line.startswith("#") else shorten_line(line))


if __name__ == "__main__":
    main()
