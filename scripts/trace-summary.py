#!/usr/bin/env python3
"""Rank the spans in a Chrome trace produced by `--features profile`.

`bevy/trace_chrome` writes one span-begin/-end record per line, which Perfetto
renders as a timeline but is awkward to answer "what costs the most" with — a
12-second capture is well over a gigabyte. This streams the file and prints a
per-span-name table instead:

    total   summed wall time inside the span
    self    total minus the time spent inside nested spans
    calls   number of times the span was entered
    /frame  calls divided by the number of frames in the capture

Times are summed across threads, and the multi-threaded executor runs each
system on a worker thread while the schedule's own span sits on the main one,
so a *schedule's* self time still includes its systems. Individual `system:`
spans are the trustworthy numbers; `--filter 'system: '` ranks just those.

Usage:
    scripts/trace-summary.py trace.json [-n 40] [--filter system]
"""

import argparse
import sys
from collections import defaultdict

# The writer emits keys in alphabetical order, so "name" is always followed by
# "ph" — that lets us slice the fields out without paying for a JSON parse per
# line (there are millions of them).
NAME_KEY = '"name":"'
PH_KEY = '","ph":"'
TS_KEY = '"ts":'
TID_KEY = '"tid":'

# Brackets one iteration of the app: used for the per-frame column.
FRAME_SPAN = "schedule: name=Main"


def parse(line):
    """-> (name, phase, tid, ts_micros), or None for non-span records."""
    n = line.find(NAME_KEY)
    if n < 0:
        return None
    p = line.find(PH_KEY, n)
    if p < 0:
        return None
    name = line[n + len(NAME_KEY) : p]
    phase = line[p + len(PH_KEY)]
    if phase not in "BE":
        return None
    t = line.rfind(TS_KEY)
    if t < 0:
        return None
    ts = float(line[t + len(TS_KEY) : line.rfind("}")])
    d = line.rfind(TID_KEY)
    tid = line[d + len(TID_KEY) : line.find(",", d)]
    return name, phase, tid, ts


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("trace", help="trace-*.json written by TRACE_CHROME")
    ap.add_argument("-n", type=int, default=40, help="rows to print")
    ap.add_argument("--filter", default="", help="only spans containing this")
    ap.add_argument(
        "--sort", choices=("self", "total", "calls"), default="self"
    )
    args = ap.parse_args()

    total = defaultdict(float)
    own = defaultdict(float)
    calls = defaultdict(int)
    # Per thread: the open spans, plus how much of each one's time has been
    # claimed by a nested span (so `self` doesn't double-count children).
    stacks = defaultdict(list)
    frames = 0
    span_start = None
    span_end = 0.0

    with open(args.trace, "r", errors="replace") as f:
        for line in f:
            rec = parse(line)
            if rec is None:
                continue
            name, phase, tid, ts = rec
            if span_start is None:
                span_start = ts
            span_end = max(span_end, ts)
            stack = stacks[tid]
            if phase == "B":
                stack.append([name, ts, 0.0])
                continue
            if not stack:
                continue  # end without a begin: capture was cut short
            _, start, children = stack.pop()
            elapsed = ts - start
            total[name] += elapsed
            own[name] += elapsed - children
            calls[name] += 1
            if stack:
                stack[-1][2] += elapsed
            if name == FRAME_SPAN:
                frames += 1

    if not calls:
        sys.exit("no spans found — was the app built with --features profile?")

    key = {"self": own, "total": total, "calls": calls}[args.sort]
    rows = [n for n in calls if args.filter in n]
    rows.sort(key=lambda n: key[n], reverse=True)

    seconds = (span_end - span_start) / 1e6
    print(f"{seconds:.1f}s of trace, {frames} frames, {len(calls)} distinct spans")
    print(f"{'self ms':>9} {'total ms':>9} {'calls':>8} {'/frame':>7}  name")
    for name in rows[: args.n]:
        per_frame = calls[name] / frames if frames else 0.0
        print(
            f"{own[name] / 1000:9.1f} {total[name] / 1000:9.1f} "
            f"{calls[name]:8d} {per_frame:7.2f}  {name}"
        )


if __name__ == "__main__":
    main()
