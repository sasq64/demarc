#!/usr/bin/env python3
"""Turn a screenshot into a pouet.net-sized JPEG.

pouet wants 400x300 and at most 64000 bytes, so this resizes with ImageMagick
and then binary-searches the JPEG quality for the largest file that still fits.
If even quality 1 is too big the image is downscaled a step at a time until it
does.

Usage:
    scripts/pouet_shot.py shot.png [-o shot.jpg] [--stretch|--crop] [--max-bytes N]
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

WIDTH, HEIGHT = 400, 300
MAX_BYTES = 64000


def magick(args: list[str]) -> None:
    exe = shutil.which("magick")
    cmd = [exe] + args if exe else ["convert"] + args
    subprocess.run(cmd, check=True)


def render(src: Path, dst: Path, geometry: str, extra: list[str], quality: int) -> int:
    """Write `src` to `dst` as a JPEG at `quality`, returning its size in bytes."""
    magick(
        [
            str(src),
            "-auto-orient",
            "-resize",
            geometry,
            *extra,
            "-strip",
            "-interlace",
            "Plane",
            "-sampling-factor",
            "4:2:0",
            "-quality",
            str(quality),
            str(dst),
        ]
    )
    return dst.stat().st_size


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("input", type=Path, help="source image (png or anything ImageMagick reads)")
    ap.add_argument("-o", "--output", type=Path, help="output jpg (default: input with .jpg suffix)")
    ap.add_argument("--max-bytes", type=int, default=MAX_BYTES, help=f"size limit (default {MAX_BYTES})")
    mode = ap.add_mutually_exclusive_group()
    mode.add_argument("--stretch", action="store_true", help="force exactly 400x300, ignoring aspect ratio")
    mode.add_argument("--crop", action="store_true", help="fill 400x300 and crop the overflow")
    args = ap.parse_args()

    if not args.input.is_file():
        print(f"no such file: {args.input}", file=sys.stderr)
        return 1

    out = args.output or args.input.with_suffix(".jpg")

    def geometry_for(w: int, h: int) -> tuple[str, list[str]]:
        if args.stretch:
            return f"{w}x{h}!", []
        if args.crop:
            return f"{w}x{h}^", ["-gravity", "center", "-extent", f"{w}x{h}"]
        # Fit inside the box; a 4:3 source lands on 400x300 exactly.
        return f"{w}x{h}", []

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = Path(tmpdir) / "try.jpg"

        for scale in (100, 90, 80, 70, 60, 50, 40):
            geometry, extra = geometry_for(WIDTH * scale // 100, HEIGHT * scale // 100)

            # Highest quality that fits, by binary search over 1..95.
            lo, hi, best = 1, 95, None
            while lo <= hi:
                mid = (lo + hi) // 2
                size = render(args.input, tmp, geometry, extra, mid)
                if size <= args.max_bytes:
                    best = (mid, size)
                    lo = mid + 1
                else:
                    hi = mid - 1

            if best is None:
                continue

            quality, size = best
            render(args.input, tmp, geometry, extra, quality)
            shutil.copyfile(tmp, out)
            dims = subprocess.run(
                ["identify", "-format", "%wx%h", str(out)], check=True, capture_output=True, text=True
            ).stdout
            print(f"{out}: {dims}, quality {quality}, {size} bytes (limit {args.max_bytes})")
            return 0

    print("cannot get under the size limit even at quality 1", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
