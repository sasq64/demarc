#!/usr/bin/env python3
"""fetch_dumps — keep the offline data dumps the databases are built from fresh.

    ./fetch_dumps.py pouet      # pouetdatadump-prods-<date>.json
    ./fetch_dumps.py demozoo    # demozoo-export.sql

Each source is asked for a newer dump at most once a day: a successful check
leaves a stamp file (.pouet_dump.checked, .demozoo_export.checked) and a run
later the same day does nothing while the stamp and the dump are both there.
--force checks regardless.

pouet lists its weekly dumps in a JSON feed; the newest prods dump is fetched
if we do not have a file of that name yet.  Older dumps are left alone --
pouet.py reads the newest one.

Demozoo publishes one export under a fixed url, so "newer" is its
Last-Modified against the mtime of our copy, which is set to that header when
the download lands.

A check that fails (no network) is a warning as long as there is a dump to
build from, so a rebuild still works offline; with no dump at all it is an
error.  Downloads are unpacked beside their target and renamed into place, so
an interrupted run never leaves a short dump behind.
"""

import argparse
import datetime
import email.utils
import glob
import gzip
import json
import lzma
import os
import shutil
import sys
import urllib.request

import pouet

POUET_FEED = "https://data.pouet.net/json.php"
POUET_STAMP = ".pouet_dump.checked"

DEMOZOO_URL = "https://data.demozoo.org/demozoo-export.sql.gz"
DEMOZOO_SQL = "demozoo-export.sql"
DEMOZOO_STAMP = ".demozoo_export.checked"

# pouet 403s the default urllib agent.
USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64) demodb/fetch_dumps"

# How each compressed dump is opened; pouet has published both.
OPENERS = {".gz": gzip.open, ".xz": lzma.open}


def request(url, method="GET"):
    return urllib.request.Request(url, method=method, headers={"User-Agent": USER_AGENT})


def checked_today(stamp):
    """Whether `stamp` was touched today, local time."""
    try:
        mtime = os.path.getmtime(stamp)
    except OSError:
        return False
    return datetime.date.fromtimestamp(mtime) == datetime.date.today()


def touch(stamp):
    with open(stamp, "a"):
        os.utime(stamp, None)


def download(url, dest):
    """Fetch `url`, unpacking it if it is compressed, into `dest`."""
    packed = dest + ".download"
    tmp = dest + ".tmp"
    try:
        with urllib.request.urlopen(request(url), timeout=60) as resp:
            total = int(resp.headers.get("Content-Length") or 0)
            done = 0
            with open(packed, "wb") as out:
                while chunk := resp.read(1 << 20):
                    out.write(chunk)
                    done += len(chunk)
                    if total:
                        print(
                            f"\r  {done >> 20}/{total >> 20} MB",
                            end="",
                            file=sys.stderr,
                            flush=True,
                        )
            print(file=sys.stderr)
            if total and done != total:
                raise OSError(f"short download: {done} of {total} bytes")
        opener = OPENERS.get(os.path.splitext(url)[1], open)
        with opener(packed, "rb") as src, open(tmp, "wb") as out:
            shutil.copyfileobj(src, out, 1 << 20)
        os.replace(tmp, dest)
    finally:
        for path in (packed, tmp):
            if os.path.exists(path):
                os.remove(path)


def update_pouet():
    """Fetch the newest pouet prods dump unless we already have it."""
    with urllib.request.urlopen(request(POUET_FEED), timeout=60) as resp:
        latest = json.load(resp)["latest"]["prods"]
    name = latest["filename"]
    if os.path.splitext(name)[1] in OPENERS:
        name = os.path.splitext(name)[0]
    if os.path.exists(name):
        print(f"pouet: {name} is the newest dump", file=sys.stderr)
        return
    print(f"pouet: new dump {name}, downloading", file=sys.stderr)
    download(latest["url"], name)


def update_demozoo():
    """Fetch the Demozoo export if it is missing or the server's is newer."""
    with urllib.request.urlopen(request(DEMOZOO_URL, "HEAD"), timeout=60) as resp:
        modified = resp.headers.get("Last-Modified")
    remote = email.utils.parsedate_to_datetime(modified).timestamp() if modified else None
    if os.path.exists(DEMOZOO_SQL):
        if remote is None:
            print("demozoo: server gives no date, keeping our export", file=sys.stderr)
            return
        if remote <= os.path.getmtime(DEMOZOO_SQL):
            print(f"demozoo: {DEMOZOO_SQL} is up to date", file=sys.stderr)
            return
    print(f"demozoo: new export ({modified}), downloading", file=sys.stderr)
    download(DEMOZOO_URL, DEMOZOO_SQL)
    if remote is not None:
        os.utime(DEMOZOO_SQL, (remote, remote))


# name -> (update function, stamp file, glob matching the dumps we hold)
SOURCES = {
    "pouet": (update_pouet, POUET_STAMP, pouet.DUMP_GLOB),
    "demozoo": (update_demozoo, DEMOZOO_STAMP, DEMOZOO_SQL),
}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("source", nargs="+", choices=sorted(SOURCES))
    ap.add_argument(
        "--force", action="store_true", help="check even if already checked today"
    )
    args = ap.parse_args()

    for source in args.source:
        update, stamp, have = SOURCES[source]
        if not args.force and checked_today(stamp) and glob.glob(have):
            print(f"{source}: already checked today", file=sys.stderr)
            continue
        try:
            update()
        except (OSError, ValueError, KeyError) as e:
            if not glob.glob(have):
                raise SystemExit(f"error: {source}: cannot fetch a dump: {e}")
            print(
                f"  WARNING: {source}: check for a newer dump failed ({e}); "
                f"using the one we have",
                file=sys.stderr,
            )
            continue
        touch(stamp)


if __name__ == "__main__":
    main()
