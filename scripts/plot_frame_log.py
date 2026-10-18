#!/usr/bin/env python3
"""Graph the CSV that demarc --frame-log writes.

    demarc demo.zip --frame-log=frames.csv
    scripts/plot_frame_log.py frames.csv            # window
    scripts/plot_frame_log.py frames.csv -o out.png # file

Four stacked plots share the time axis: the audio ring fill against the
audio_buf_min/max window, the frame delta against the core's reported frame
time, the number of finished frames waiting in the core's update channel, and
how busy the core's worker thread was.

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

FILL = "#2a78d6"
DELTA = "#eb6834"
QUEUED = "#1baf7a"
CPU = "#4a3aa7"
LIMIT = "#52514e"


def read(path):
    with open(path, newline="") as f:
        rows = list(csv.DictReader(f))
    if not rows:
        sys.exit(f"{path}: no frames logged")
    emus = {}
    for r in rows:
        c = emus.setdefault(int(r["emu"]), {k: [] for k in r if k != "emu"})
        for k, v in r.items():
            if k != "emu":
                c[k].append(float(v))
    return emus


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("csv", help="file written by --frame-log")
    ap.add_argument("-o", "--out", help="write a PNG instead of opening a window")
    ap.add_argument("-e", "--emu", type=int, help="only this emulator (the emu column); default: all")
    args = ap.parse_args()

    if args.out:
        matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    emus = read(args.csv)
    if args.emu is not None:
        if args.emu not in emus:
            sys.exit(f"no emu {args.emu} in {args.csv}; have {sorted(emus)}")
        emus = {args.emu: emus[args.emu]}

    fig, (ax_fill, ax_delta, ax_queue, ax_cpu) = plt.subplots(4, sharex=True, figsize=(14, 11),
                                                              height_ratios=(3, 2, 1, 1.5))
    for emu, c in emus.items():
        tag = f" (emu {emu})" if len(emus) > 1 else ""
        t = c["time"]

        ax_fill.fill_between(t, c["buf_min"], c["buf_max"], step="post", color=LIMIT, alpha=0.12, lw=0)
        ax_fill.step(t, c["buf_min"], where="post", color=LIMIT, lw=1, ls="--", label="audio_buf_min" + tag)
        ax_fill.step(t, c["buf_max"], where="post", color=LIMIT, lw=1, label="audio_buf_max" + tag)
        ax_fill.plot(t, c["fill"], color=FILL, lw=1, label="fill" + tag)

        frame_ms = [1000.0 / f if f > 0 else float("nan") for f in c["fps"]]
        ax_delta.plot(t, [d * 1000.0 for d in c["delta"]], color=DELTA, lw=1, label="frame delta" + tag)
        ax_delta.step(t, frame_ms, where="post", color=LIMIT, lw=1, ls="--", label="1 / reported fps" + tag)

        ax_queue.step(t, c["queued"], where="post", color=QUEUED, lw=1, label="queued frames" + tag)

        if "cpu" in c:
            ax_cpu.plot(t, [v * 100.0 for v in c["cpu"]], color=CPU, lw=1, label="worker busy" + tag)

        fps = sorted(set(round(f, 3) for f in c["fps"] if f > 0))
        print(f"emu {emu}: {len(t)} frames, reported fps {', '.join(map(str, fps)) or 'none'}")

    ax_fill.set_ylabel("audio ring fill (samples)")
    ax_delta.set_ylabel("frame delta (ms)")
    ax_queue.set_ylabel("update channel")
    ax_queue.yaxis.get_major_locator().set_params(integer=True)
    ax_cpu.set_ylabel("core cpu (%)")
    ax_cpu.set_ylim(0, 105)
    ax_cpu.set_xlabel("seconds")
    for ax in (ax_fill, ax_delta, ax_queue, ax_cpu):
        ax.grid(alpha=0.3)
        if ax is not ax_cpu:
            ax.set_ylim(bottom=0)
        ax.legend(loc="upper right", fontsize="small", ncols=2)
    fig.tight_layout()

    out = args.out
    if not out and plt.get_backend().lower().startswith("agg"):
        out = os.path.splitext(args.csv)[0] + ".png"
        print("matplotlib has no interactive backend here (try installing python-tk)", file=sys.stderr)

    if out:
        fig.savefig(out, dpi=110)
        print(f"wrote {out}")
    else:
        plt.show()


if __name__ == "__main__":
    main()
