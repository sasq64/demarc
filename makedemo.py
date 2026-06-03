#!/usr/bin/env python3
"""Bundle files into a "{group} - {title}" folder with a demo.m3u playlist.

Usage:
    makedemo.py file1 [file2 ...] key="value" [key="value" ...]

Arguments may be given in any order. Anything matching key="value" (or
key=value) is treated as metadata; everything else is treated as a file to
move into the new folder.
"""

import os
import re
import shutil
import sys

META_RE = re.compile(r'^([A-Za-z_][\w-]*)=(.*)$')


def parse_args(args):
    files = []
    meta = {}
    for arg in args:
        m = META_RE.match(arg)
        if m:
            key, value = m.group(1), m.group(2)
            # Strip a single layer of surrounding quotes if present.
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            meta[key] = value
        else:
            files.append(arg)
    return files, meta


def folder_name(meta):
    group = meta.get("group")
    title = meta.get("title")
    if group and title:
        return f"{group} - {title}"
    return title or group or "demo"


def main(argv):
    files, meta = parse_args(argv)

    for f in files:
        if not os.path.exists(f):
            sys.exit(f"error: file not found: {f}")

    folder = folder_name(meta)
    os.makedirs(folder, exist_ok=True)

    moved = []
    for f in files:
        dest = os.path.join(folder, os.path.basename(f))
        shutil.move(f, dest)
        moved.append(os.path.basename(f))

    # Build the EXTINF metadata in a stable, readable order.
    ordered = list(meta.items())
    info = " ".join(f'{k}="{v}"' for k, v in ordered)

    m3u_path = os.path.join(folder, "demo.m3u")
    with open(m3u_path, "w") as fp:
        fp.write("#EXTM3U\n")
        fp.write(f"#EXTINF:-1 {info}\n" if info else "#EXTINF:-1\n")
        for name in moved:
            fp.write(name + "\n")

    print(f"Created '{folder}' with {len(moved)} file(s) and demo.m3u")


if __name__ == "__main__":
    main(sys.argv[1:])
