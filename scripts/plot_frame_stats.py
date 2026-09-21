#!/usr/bin/env python3
"""Graph the CSV that DEMARC_FRAME_STATS writes.

Run demarc with the variable set to get one line per emulated frame:

    DEMARC_FRAME_STATS=stats.csv demarc demo.zip --headless --remote-control s.lua
    scripts/plot_frame_stats.py stats.csv            # window
    scripts/plot_frame_stats.py stats.csv -o out.png # file

Three stacked plots share the frame axis: the per-frame difference with its
aggregate on top, how much the picture differs from its own average colour, and
that average colour itself. The point is to eyeball where a demo is actually
running against where it sits on a loader, a crash box or a black screen.

Needs matplotlib:  python3 -m venv .venv && .venv/bin/pip install matplotlib
"""
# /// script
# dependencies = ["matplotlib"]
# ///

import argparse
import csv
import os
import sys

try:
    import matplotlib
except ImportError:
    sys.exit("matplotlib is missing: python3 -m venv .venv && .venv/bin/pip install matplotlib")


def read(path):
    with open(path, newline="") as f:
        rows = list(csv.DictReader(f))
    if not rows:
        sys.exit(f"{path}: no frames logged")
    cols = {k: [float(r[k]) for r in rows] for k in rows[0]}
    return cols


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv", nargs="+", help="file(s) written by DEMARC_FRAME_STATS")
    ap.add_argument("-o", "--out", help="write a PNG instead of opening a window")
    ap.add_argument("-x", "--time", action="store_true", help="plot against seconds, not frame number")
    ap.add_argument("--log", action="store_true", help="log scale for the motion plot")
    ap.add_argument("--idle", type=float, default=0.002,
                    help="draw a threshold line at this aggregated_diff (default: %(default)s)")
    args = ap.parse_args()

    if args.out:
        matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    fig, (ax_diff, ax_color, ax_avg) = plt.subplots(3, sharex=True, figsize=(14, 9))

    for path in args.csv:
        c = read(path)
        x = [t / 1000.0 for t in c["time_ms"]] if args.time else c["frame"]
        tag = f" ({path})" if len(args.csv) > 1 else ""

        ax_diff.plot(x, c["frame_diff"], lw=0.6, alpha=0.5, label="frame_diff" + tag)
        ax_diff.plot(x, c["aggregated_diff"], lw=1.6, label="aggregated_diff" + tag)
        ax_color.plot(x, c["color_diff"], lw=0.8, label="color_diff" + tag)
        for ch, color in (("avg_r", "red"), ("avg_g", "green"), ("avg_b", "blue")):
            ax_avg.plot(x, c[ch], lw=0.8, color=color, alpha=0.8, label=ch + tag)

    if args.idle > 0:
        ax_diff.axhline(args.idle, color="k", ls="--", lw=0.8, label=f"idle threshold {args.idle}")

    ax_diff.set_ylabel("motion")
    if args.log:
        ax_diff.set_yscale("log")
    ax_color.set_ylabel("color spread\n(0 = flat, 0.5 = max)")
    ax_avg.set_ylabel("average colour")
    ax_avg.set_ylim(0, 255)
    ax_avg.set_xlabel("seconds" if args.time else "frame")
    for ax in (ax_diff, ax_color, ax_avg):
        ax.grid(alpha=0.3)
        ax.legend(loc="upper right", fontsize="small", ncols=2)
    fig.tight_layout()

    out = args.out
    if not out and plt.get_backend().lower().startswith("agg"):
        # No GUI backend is installed (python-tk, PyQt, ...), so a window is not
        # on offer whatever we do; write the picture next to the log instead.
        out = os.path.splitext(args.csv[0])[0] + ".png"
        print("matplotlib has no interactive backend here (try installing python-tk)", file=sys.stderr)

    if out:
        fig.savefig(out, dpi=110)
        print(f"wrote {out}")
    else:
        plt.show()


if __name__ == "__main__":
    main()
