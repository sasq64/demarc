#!/usr/bin/env python3
"""pouet -- read the extra numbers off pouet.net's production API.

demozoo.py already knows, for every release Demozoo links to pouet.net, which
prod id that is (the `PouetProduction` link class).  The toplists it fetches
only cover the top 64 per platform, so anything below that gets no pouet data
at all.  This module fills that gap: given a prod id it fetches

    https://api.pouet.net/v1/prod/?id=<id>

and pulls out what demarc cares about -- the CDC count, the popularity
percentage, the thumb-up/ok/down votes and any awards the prod won or was
nominated for.  The API answers with ~5KB of JSON where prod.php is ~100KB of
HTML for the same numbers, and it needs no html parser.

pouet.net also publishes a full data dump of every prod --
pouetdatadump-prods-<date>.json, ~160MB of the same rows the API serves.  When
one is lying next to this file we answer out of it instead: a lookup costs a
seek and one json.loads, not a request, which turns an export of fifty thousand
prods from an afternoon of polite fetching into a few seconds.  The dump is
only read once, lazily, the first time somebody asks for a prod (see
load_dump), and it is a *snapshot*: pass refresh=True to go to the live API for
today's vote counts.

Prods the dump does not have -- newer than the dump date, mostly -- still go to
the API, and responses are cached under .pouetcache/prod-<id>.json, so a rerun
is offline and pouet.net is only asked once per prod.

    >>> get_prod(981)
    PouetProd(id=981, name='Hardwired', cdc=24, popularity=84.6..., rulez=292,
              isok=16, sucks=0, rank=47, awards=[])

Run as a script to dump one or more prods:  ./pouet.py 981 51983
"""

import argparse
import glob
import json
import os
import re
import sys
import time
from dataclasses import dataclass, field
from urllib.request import Request, urlopen

API_URL = "https://api.pouet.net/v1/prod/?id={id}"

# One file per prod; delete the directory (or pass refresh=True) to pick up
# newer vote counts.  Kept separate from demozoo.py's .pouet_cache, which holds
# the per-platform toplist pages.
CACHE_DIR = ".pouetcache"

# The offline data dump.  Looked for next to this file and in the working
# directory; the names sort by date, so the newest one wins.  $POUET_DUMP
# overrides both, and setting it to an empty string turns the dump off.
DUMP_GLOB = "pouetdatadump-prods-*.json"
DUMP_ENV = "POUET_DUMP"


@dataclass
class PouetAward:
    """One award row.  The API names awards by category id only -- there is no
    endpoint that maps those to 'The Meteoriks - Best Low-End Production', so
    the id is what we can honestly report.  `type` is pouet's own wording,
    'winner' or 'nominee'."""
    category_id: int
    type: str

    def __str__(self):
        return f"{self.type}:{self.category_id}"


@dataclass
class PouetProd:
    """What the API tells us about one prod.  `popularity` is the percentage
    the site rounds down to '84%' (see popularity_pct), `rank` its alltime-top
    position (0 if unranked), `awards` the award rows in API order."""
    id: int
    name: str = ""
    cdc: int = 0
    popularity: float = 0.0
    rulez: int = 0
    isok: int = 0
    sucks: int = 0
    rank: int = 0
    awards: list[PouetAward] = field(default_factory=list)

    @property
    def thumbs(self) -> int:
        """Net thumbs, the number demozoo.py's toplist rows call `thumbs`."""
        return self.rulez

    @property
    def popularity_pct(self) -> int:
        """Popularity as the site shows it: 73.945 -> 73."""
        return int(self.popularity)

    @property
    def winner_ids(self) -> list[int]:
        """Category ids this prod won, in API order."""
        return [a.category_id for a in self.awards if a.type == "winner"]

    @property
    def nominee_ids(self) -> list[int]:
        """Category ids this prod was only nominated for, in API order."""
        return [a.category_id for a in self.awards if a.type == "nominee"]


_INT_RE = re.compile(r"-?\d+")


def _int(value, default=0):
    """`value` as an int, however the API spelled it ('109', 109, None).  Vote
    counts come back as strings, cdc as a number, absent fields as null."""
    if isinstance(value, bool) or value is None:
        return default
    if isinstance(value, (int, float)):
        return int(value)
    m = _INT_RE.search(str(value))
    return int(m.group()) if m else default


def _float(value, default=0.0):
    try:
        return float(value)
    except (TypeError, ValueError):
        return default


def fetch_page(url):
    """GET `url` as text.  Pouet 403s the default urllib agent, so pose as a
    browser the way demozoo.py and bitworld_scrape.py do."""
    req = Request(url, headers={"User-Agent": "Mozilla/5.0"})
    with urlopen(req, timeout=60) as resp:
        return resp.read().decode("utf-8", errors="replace")


def cache_path(prod_id, cache_dir=CACHE_DIR):
    """Where the response for `prod_id` is cached."""
    return os.path.join(cache_dir, f"prod-{int(prod_id)}.json")


def is_cached(prod_id, cache_dir=CACHE_DIR):
    """True if get_prod(`prod_id`) would answer without hitting the network --
    because the data dump has the prod, or because we fetched it before.
    Callers pacing a long run use this to sleep only between real fetches."""
    return in_dump(prod_id) or os.path.exists(cache_path(prod_id, cache_dir))


def prod_json(prod_id, cache_dir=CACHE_DIR, refresh=False):
    """The API response for `prod_id` as text, from the disk cache if we have
    it.  Error responses are cached too: pouet answers 200 with {"error":true}
    for a prod that does not exist, and asking again would not change that."""
    path = cache_path(prod_id, cache_dir)
    if not refresh and os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            return f.read()
    text = fetch_page(API_URL.format(id=int(prod_id)))
    os.makedirs(cache_dir, exist_ok=True)
    # Write-then-rename, as elsewhere here: a half-written response left by an
    # interrupted fetch would otherwise be cached as if it were the real thing.
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(text)
    os.replace(tmp, path)
    return text


def parse_prod(text, prod_id: int = 0) -> PouetProd | None:
    """Pull the stats out of an API response (json text, or the decoded dict).

    The fields we want sit at the top level of `prod`:

        "voteup": "109", "votepig": "3", "votedown": "0",
        "cdc": 1, "popularity": 73.945, "rank": "767",
        "awards": [{"categoryID": "40", "awardType": "nominee"}, ...]

    Returns None when pouet has no such prod -- it answers HTTP 200 with
    {"error": true} rather than a 404 -- or when the body is not the JSON we
    expect, so a caller can tell "no data" from a broken run either way.
    """
    if isinstance(text, (dict, list)):
        data = text
    else:
        try:
            data = json.loads(text)
        except ValueError:
            return None
    if not isinstance(data, dict) or not data.get("success"):
        return None
    p = data.get("prod")
    if not isinstance(p, dict):
        return None
    return parse_row(p, prod_id)


def parse_row(p: dict, prod_id: int = 0) -> PouetProd:
    """One prod row -> PouetProd.  The API wraps this in {"success", "prod"}
    and the data dump lists the very same rows under "prods", so both go
    through here."""
    awards = []
    for a in p.get("awards") or []:
        if isinstance(a, dict):
            awards.append(PouetAward(category_id=_int(a.get("categoryID")),
                                     type=str(a.get("awardType") or "")))

    return PouetProd(
        id=_int(p.get("id"), int(prod_id)),
        name=str(p.get("name") or ""),
        cdc=_int(p.get("cdc")),
        popularity=_float(p.get("popularity")),
        rulez=_int(p.get("voteup")),
        isok=_int(p.get("votepig")),
        sucks=_int(p.get("votedown")),
        rank=_int(p.get("rank")),
        awards=awards,
    )


# ---------------------------------------------------------------------------
# The offline data dump.
#
# The file is one long json object, but pouet writes it with every prod on its
# own line:
#
#     {"dump_date":"2026-08-19 04:30:01","prods":[
#     {"id":"1","name":"Astral Blur",...},
#     ...
#     ]}
#
# so we never decode the whole 160MB.  Loading it means scanning the lines for
# the leading "id" and remembering each one's byte offset -- about a tenth of a
# second and 5MB for a hundred thousand prods -- after which a lookup is a seek
# and one json.loads of a single line.  If pouet ever reformats the dump the
# scan simply finds nothing, and we say so once and fall back to the API.
# ---------------------------------------------------------------------------

_DUMP_ID_RE = re.compile(rb'^\{"id":"?(\d+)"?')

# (path, {prod id: byte offset}, open file) for the loaded dump, or None once
# we have looked and found nothing.  _dump_loaded keeps us from looking twice.
_dump = None
_dump_loaded = False


def find_dump(path=None):
    """Where the data dump is, or None.  An explicit `path` (or $POUET_DUMP)
    wins; otherwise the newest DUMP_GLOB match beside this file or in the
    working directory, the names being datestamped and so sorting by age."""
    if path is None:
        path = os.environ.get(DUMP_ENV)
    if path is not None:
        # Empty $POUET_DUMP is how you say "no dump, use the API".
        return path or None
    found = []
    for d in (os.path.dirname(os.path.abspath(__file__)), os.getcwd()):
        found += glob.glob(os.path.join(d, DUMP_GLOB))
    return max(found, key=os.path.basename) if found else None


def load_dump(path=None):
    """Index the data dump, once -- every lookup after the first reuses the
    index.  Returns (path, index, file) or None when there is no dump to read;
    a missing or unreadable file is not an error, it just means the API answers
    the lookups."""
    global _dump, _dump_loaded
    if path:
        path = os.path.abspath(path)
    # Asking again for the dump we already hold -- by name or by no name at
    # all -- is what a per-prod caller does, and must not reindex it.
    if _dump_loaded and (path is None or (_dump and path == _dump[0])):
        return _dump
    if path is None:
        path = find_dump()
        if path:
            path = os.path.abspath(path)
    _dump_loaded = True
    _dump = None
    if not path:
        return None
    try:
        f = open(path, "rb")
    except OSError as e:
        print(f"  WARNING: cannot read pouet dump {path}: {e}", file=sys.stderr)
        return None

    index = {}
    offset = 0
    for line in f:
        m = _DUMP_ID_RE.match(line)
        if m:
            index[int(m.group(1))] = offset
        offset += len(line)
    if not index:
        print(f"  WARNING: no prods found in pouet dump {path}; "
              f"falling back to the API", file=sys.stderr)
        f.close()
        return None
    print(f"Pouet data dump: {len(index)} prods from "
          f"{os.path.basename(path)}", file=sys.stderr)
    _dump = (path, index, f)
    return _dump


def in_dump(prod_id, path=None) -> bool:
    """True if dump_prod(`prod_id`) would answer.  Callers pacing a run use
    this, like is_cached, to sleep only when they are really fetching."""
    dump = load_dump(path)
    return bool(dump) and int(prod_id) in dump[1]


def dump_prod(prod_id, path=None) -> PouetProd | None:
    """`prod_id` out of the data dump, or None if there is no dump or it does
    not have that prod (anything added after the dump date, mostly)."""
    dump = load_dump(path)
    if not dump:
        return None
    _, index, f = dump
    offset = index.get(int(prod_id))
    if offset is None:
        return None
    f.seek(offset)
    # Every prod line but the last ends in a comma before the closing "]}".
    line = f.readline().strip().rstrip(b",")
    try:
        row = json.loads(line)
    except ValueError:
        return None
    return parse_row(row, prod_id) if isinstance(row, dict) else None


def get_prod(prod_id, cache_dir=CACHE_DIR, refresh=False) -> PouetProd | None:
    """One prod, from the data dump if it has it, else fetched (or read back
    from the cache) over the API.  None if pouet has no such prod; raises
    OSError (urllib's HTTPError included) if the fetch fails.

    `refresh` skips the dump as well as the cache: the dump is a snapshot, so
    it is also what you refresh away from when you want today's votes."""
    if not refresh:
        prod = dump_prod(prod_id)
        if prod is not None:
            return prod
    return parse_prod(prod_json(prod_id, cache_dir, refresh), prod_id)


def get_prods(prod_ids, cache_dir=CACHE_DIR, refresh=False,
              delay=0.0) -> dict[int, PouetProd]:
    """{prod id: PouetProd} for the ids we could read.  A prod that does not
    exist or whose fetch fails is warned about and skipped rather than failing
    the whole run -- this data is an extra, not the point of the export.

    `delay` seconds are slept after each *fetch*, so a long run does not hammer
    pouet.net; prods already cached go at full speed."""
    out = {}
    for prod_id in prod_ids:
        cached = not refresh and is_cached(prod_id, cache_dir)
        try:
            prod = get_prod(prod_id, cache_dir, refresh)
        except OSError as e:
            print(f"  WARNING: no pouet prod {prod_id}: {e}", file=sys.stderr)
            continue
        if prod is not None:
            out[int(prod_id)] = prod
        if delay and not cached:
            time.sleep(delay)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("ids", nargs="+", type=int, help="pouet prod ids")
    ap.add_argument("--cache-dir", default=CACHE_DIR,
                    help=f"where responses are cached (default: {CACHE_DIR})")
    ap.add_argument("--refresh", action="store_true",
                    help="refetch instead of using the dump or the cache")
    ap.add_argument("--dump", default=None,
                    help=f"the data dump to read ({DUMP_GLOB} beside this "
                         f"script or in the working directory by default)")
    ap.add_argument("--no-dump", action="store_true",
                    help="ignore the data dump and ask the API")
    args = ap.parse_args()

    if not args.refresh:
        load_dump("" if args.no_dump else args.dump)

    for prod_id in args.ids:
        prod = get_prod(prod_id, args.cache_dir, args.refresh)
        if prod is None:
            print(f"{prod_id}: no such prod", file=sys.stderr)
            continue
        print(f"{prod.id} {prod.name}: cdc={prod.cdc} "
              f"popularity={prod.popularity_pct}% rulez={prod.rulez} "
              f"isok={prod.isok} sucks={prod.sucks} rank={prod.rank or '-'}")
        for award in prod.awards:
            print(f"  award: {award}")


if __name__ == "__main__":
    main()
