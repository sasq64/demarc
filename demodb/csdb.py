#!/usr/bin/env python3
"""csdb — download C64 release metadata from CSDb and write a csdb.txt.

The CSDb "releases_extract" API returns releases in id ranges of at most 1000.
We walk the id range in 1000-wide blocks, caching each block's raw JSON on disk
so re-runs don't refetch data we already have.  CSDb gets new releases every
day, so unless --max says otherwise the walk has no fixed end: it goes on until
a block comes back empty, and the blocks at the end of the cache -- the ones
new ids can still land in -- are refetched on every run (see load_releases).
We then emit a
csdb.txt in the named-field db format read by src/files.rs, the same one
bitworld.txt and demozoo.txt use — a `# Platform:C64` header followed by one
tab-separated line per release:

    id:1<TAB>title:Embryo<TAB>author:Padua<TAB>date:2001<TAB>party:Mekka &
    Symposium 2001;C64 Demo;3<TAB>category:Demo<TAB>download:ftp://...

A field with nothing after its colon says nothing, so it is left out of the
line altogether -- an absent field and an empty one mean the same thing, and
demarc skips both when it reads the file back.

Every line is a C64 release, so the platform is named once in the header rather
than per line and there is no `platform:` field.  `category` is CSDb's release
type with that same "C64 " prefix taken off ("C64 One-File Demo" -> "One-File
Demo"); the handful of types naming something else ("C128 Release", "BBS
Software") keep their full name, since with no platform field there is nowhere
else to say so.

`party` is the event name, and where CSDb records the release as a competition
entry it carries the compo and the place in it too, separated by ';'
("party:Floppy 2001;C64 Graphics;6" came sixth in Floppy 2001's C64 Graphics
compo).  An entry CSDb has no placing for leaves that part empty but keeps the
separators, so the parts stay in fixed positions (see party_field).

`download` is every download link CSDb lists, semicolon-separated and best
first as CSDb orders them; demarc downloads the first and keeps the rest as
fallbacks.  Links demarc cannot run are dropped, and with them any release
left without one: the filtering is demozoo.py's, imported from there so the
two databases agree on what counts as a usable url (DOWNLOAD_BLACKLIST for the
extensions demarc has no business launching, url_usable for the urls it cannot
read back out of the field).

`rating` is CSDb's "Weighted Average" for the release, as its toplists print it
("rating:9.73").  The releases_extract API does not carry it, so it is scraped
from the per-type toplist pages (toplist.php?type=release&subtype=(1) and so on
for every release type CSDb offers), each cached on disk beside the id blocks.
A toplist only lists releases with enough votes to rank -- a few thousand of
them -- so most lines have no rating field at all.

Usage:
    ./csdb.py                       # everything CSDb has
    ./csdb.py --min 1 --max 263500  # a fixed id range
"""

import argparse
import json
import re
import sys
import time
import urllib.parse
import urllib.request
from collections import Counter
from pathlib import Path

# demozoo.py owns the rules for which download urls demarc can actually use.
# They are not Demozoo-specific -- they are about what demarc does with the
# field -- so we import them rather than keep a second copy that can drift.
from demozoo import download_allowed, url_usable

API = "https://csdb.dk/misc/releases_extract.php"
BLOCK = 1000  # API requires releasemaxid - releaseminid <= 1000
PLATFORM = "C64"  # header platform, stripped off every entry's category

TOPLIST = "https://csdb.dk/toplist.php"
# The type whose toplist we fetch first: any of them carries the `subtype`
# dropdown naming all the others, so this is only a starting point.
SEED_TYPE = 1  # C64 Demo

_RELEASE_ID_RE = re.compile(r"/release/\?id=(\d+)")
_SUBTYPE_RE = re.compile(r"\((\d+)\)")


def fetch_block(min_id: int, max_id: int, release_type: int, retries: int = 3) -> list:
    """Fetch one id block from the API, returning its list of releases."""
    query = urllib.parse.urlencode(
        {
            # "releasetypeid": release_type,
            "releaseminid": min_id,
            "releasemaxid": max_id,
        }
    )
    url = f"{API}?{query}"
    for attempt in range(1, retries + 1):
        try:
            with urllib.request.urlopen(url, timeout=60) as resp:
                data = json.load(resp)
        except Exception as err:  # network / decode errors: retry a few times
            if attempt == retries:
                raise
            print(f"  retry {attempt} after error: {err}", file=sys.stderr)
            time.sleep(2 * attempt)
            continue
        if isinstance(data, dict) and "error" in data:
            raise RuntimeError(f"API error: {data['error']}")
        return data.get("releases", []) if isinstance(data, dict) else data
    return []


def block_path(cache_dir: Path, min_id: int, max_id: int) -> Path:
    """Where one id block is cached."""
    return cache_dir / f"block_{min_id}_{max_id}.json"


def load_block(
    cache_dir: Path, min_id: int, max_id: int, release_type: int, refresh: bool = False
) -> list:
    """Return a block's releases from cache, fetching and caching on a miss.

    `refresh` skips the cached copy and fetches the block again.  A refresh
    that comes back empty is not cached: it is the probe past the last release,
    and a file there would only be noise.
    """
    cache_file = block_path(cache_dir, min_id, max_id)
    if not refresh and cache_file.exists():
        releases = json.loads(cache_file.read_text())
        print(f"cached  {min_id}-{max_id}: {len(releases)} releases")
        return releases
    print(f"fetch   {min_id}-{max_id} ...", end="", flush=True)
    releases = fetch_block(min_id, max_id, release_type)
    if releases or not refresh:
        # Write-then-rename: a block cut short by an interrupted run would be
        # read back as the real thing.
        tmp = cache_file.with_name(cache_file.name + ".tmp")
        tmp.write_text(json.dumps(releases, ensure_ascii=False))
        tmp.replace(cache_file)
    print(f" {len(releases)} releases")
    time.sleep(0.5)  # be gentle on the server
    return releases


def load_releases(
    cache_dir: Path, min_id: int, max_id: int | None, release_type: int
) -> dict[int, dict]:
    """{release id: release} over the id range, one block at a time.

    With a `max_id` the range is fixed and every block is cached for good.

    Without one the walk runs until CSDb has no more ids -- until a block comes
    back empty.  A cached block is only trusted once the block after it has
    been cached too: that is the proof CSDb's ids had moved past it when it was
    fetched.  The last cached block and everything beyond it is fetched again
    each run, since that is where new releases turn up.

    If such a fetch fails (no network) and the block is in the cache, the
    cached copy is used with a warning and the walk ends there, so a rebuild
    still works offline.  With no cached copy the error stands.
    """
    releases: dict[int, dict] = {}

    def add(block: list) -> None:
        for rel in block:
            rid = rel.get("id")
            if rid is not None:
                releases[rid] = rel  # dedupe overlapping block boundaries

    if max_id is not None:
        for start in range(min_id, max_id + 1, BLOCK):
            end = min(start + BLOCK, max_id)
            add(load_block(cache_dir, start, end, release_type))
        return releases

    start = min_id
    while True:
        end = start + BLOCK
        if block_path(cache_dir, end, end + BLOCK).exists():
            add(load_block(cache_dir, start, end, release_type))
        else:
            try:
                block = load_block(cache_dir, start, end, release_type, refresh=True)
            except (OSError, ValueError) as err:  # network / decode errors
                print(" failed")
                if not block_path(cache_dir, start, end).exists():
                    raise
                print(
                    f"  WARNING: cannot refresh {start}-{end} ({err}); using the "
                    f"cached copy, newer releases are missing",
                    file=sys.stderr,
                )
                add(load_block(cache_dir, start, end, release_type))
                break
            if not block:
                break
            add(block)
        start = end
    return releases


def toplist_html(cache_dir: Path, type_id: int, refresh: bool = False) -> str:
    """The toplist page for one release type, from the disk cache if we have it."""
    path = cache_dir / f"toplist_{type_id}.html"
    if not refresh and path.exists():
        return path.read_text(encoding="utf-8")
    query = urllib.parse.urlencode({"type": "release", "subtype": f"({type_id})"})
    with urllib.request.urlopen(f"{TOPLIST}?{query}", timeout=60) as resp:
        html = resp.read().decode("utf-8", errors="replace")
    # Write-then-rename, as with everything else cached here: a half-written
    # page left by an interrupted fetch would be read back as the real thing.
    tmp = path.with_name(path.name + ".tmp")
    tmp.write_text(html, encoding="utf-8")
    tmp.replace(path)
    time.sleep(0.5)  # be gentle on the server
    return html


def release_types(cache_dir: Path, refresh: bool = False) -> dict[int, str]:
    """{release type id: name} for every type CSDb has a toplist for.

    Each toplist page carries a `subtype` dropdown listing all of them --
    `<option value="(2)">C64 One-File Demo</option>` -- so one page names the
    rest and we never have to keep our own copy of the list.  Should CSDb ever
    stop printing that dropdown we are left with the seed type alone, which
    costs ratings but not the export.
    """
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(toplist_html(cache_dir, SEED_TYPE, refresh), "html.parser")
    select = soup.find("select", attrs={"name": "subtype"})
    types: dict[int, str] = {}
    for option in select.find_all("option") if select else []:
        match = _SUBTYPE_RE.fullmatch((option.get("value") or "").strip())
        if match:
            types[int(match.group(1))] = option.get_text(strip=True)
    if not types:
        print("  WARNING: no subtype list on the toplist page", file=sys.stderr)
        types[SEED_TYPE] = str(SEED_TYPE)
    return types


def parse_toplist(html: str) -> dict[int, str]:
    """{release id: weighted average} out of one toplist page.

    The page is a single table, one row per release, under a header row whose
    third column is `<b>Weighted Average</b>`:

        <td align="right">1</td>
        <td><a href="/release/?id=232976">Next Level</a> by <a ...>Performers</a></td>
        <td align="center"><font size="1">9.73</font></td>

    The rating is kept as the page prints it rather than as a number, so the
    field says exactly what CSDb says.  Rows without a release link (the header
    itself, and any layout row) are skipped rather than guessed at, and a type
    nobody has rated has no table at all, which reads as no ratings.
    """
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(html, "html.parser")
    header = soup.find("b", string="Weighted Average")
    table = header.find_parent("table") if header else None
    if table is None:
        return {}

    ratings: dict[int, str] = {}
    for row in table.find_all("tr"):
        cells = row.find_all("td")
        if len(cells) < 3:
            continue
        link = cells[1].find("a", href=_RELEASE_ID_RE)
        if link is None:
            continue
        rating = cells[2].get_text(strip=True)
        try:
            float(rating)
        except ValueError:
            continue
        match = _RELEASE_ID_RE.search(link["href"])
        if match:
            ratings[int(match.group(1))] = rating
    return ratings


def load_ratings(cache_dir: Path, refresh: bool = False) -> dict[int, str]:
    """{release id: weighted average} over every release type's toplist.

    A release has one type, so it appears on one list and the maps never
    disagree; the first one to name an id keeps it either way.  A type whose
    page cannot be fetched is warned about and skipped rather than failing the
    whole export -- the rating is an extra, not the point of the file.
    """
    ratings: dict[int, str] = {}
    for type_id, name in sorted(release_types(cache_dir, refresh).items()):
        try:
            rows = parse_toplist(toplist_html(cache_dir, type_id, refresh))
        except OSError as err:
            print(f"  WARNING: no toplist for {name}: {err}", file=sys.stderr)
            continue
        print(f"toplist {name}: {len(rows)} rated releases")
        for rid, rating in rows.items():
            ratings.setdefault(rid, rating)
    print(f"Ratings: {len(ratings)} releases", file=sys.stderr)
    return ratings


def group_names(release: dict) -> str:
    """Comma-joined releasing groups, falling back to sceners if none.

    For "C64 Graphics" / "C64 Music" releases the "group" is instead the
    artist: the handle(s) credited with credittype "Graphics" / "Music".
    """
    credit_type = {"C64 Graphics": "Graphics", "C64 Music": "Music"}.get(
        release.get("type")
    )
    if credit_type:
        names = [
            c.get("handle", "")
            for c in release.get("credits") or []
            if c.get("credittype") == credit_type
        ]
        if names:
            return ", ".join(n for n in names if n)
    by = release.get("released_by") or {}
    names = [g.get("name", "") for g in by.get("groups") or []]
    if not names:
        names = [s.get("name", s.get("handle", "")) for s in by.get("sceners") or []]
    return ", ".join(n for n in names if n)


def category(release: dict) -> str:
    """A CSDb release type as the `category:` field spells it.

    Types read "C64 Demo", "C64 One-File Demo" and so on; the platform lives in
    the `# Platform:C64` header, so it's stripped from the category here.  The
    few types naming something else ("C128 Release", "BBS Software") are left
    whole -- there is no per-line platform field to correct the header with, so
    the type name is the only place that difference is recorded.
    """
    demo_type = release.get("type", "")
    rest = demo_type.removeprefix(PLATFORM).lstrip()
    if rest != demo_type:
        return rest or "Release"
    return demo_type


def party_field(release: dict) -> str:
    """The `party:` value: the event name, and for a competition entry the
    compo and the place in it as well -- 'Floppy 2001;C64 Graphics;6'.

    ';' separates the three parts, so one is folded to ',' in a name that
    carries it.  An entry CSDb lists in a compo but has no placing for (some
    2k of them) keeps both separators and ends in ';', so the compo is still
    readable and the parts stay in fixed positions; an event with no compo at
    all is the bare name, with no ';' in it.

    The place is CSDb's `compo_ranking` as it stands -- a string, so a consumer
    that wants an integer has to cope with whatever CSDb puts there.
    """
    name = (release.get("event_name") or "").strip()
    if not name:
        return ""
    fold = lambda v: str(v or "").strip().replace(";", ",")
    compo = fold(release.get("compo_name"))
    if not compo:
        return fold(name)
    return ";".join([fold(name), compo, fold(release.get("compo_ranking"))])


def downloads(release: dict) -> list[str]:
    """The release's download links, in CSDb's order, minus the ones demarc
    cannot run: a blacklisted extension or a url it cannot read back."""
    seen = set()
    urls = []
    for url in release.get("download_links") or []:
        url = (url or "").strip()
        if not url or url in seen:
            continue
        seen.add(url)
        if download_allowed(url) and url_usable(url):
            urls.append(url)
    return urls


def clean(value: str) -> str:
    """Strip the separators the db format reserves out of a free-text value.

    A value may hold ':' freely, since only the first one separates key from
    value; tabs and newlines would end the field or the line.
    """
    return value.replace("\t", " ").replace("\n", " ").replace("\r", " ").strip()


def to_row(release: dict, urls: list[str], rating: str = "") -> str:
    """Format one release as a named-field, tab-separated csdb.txt line."""
    fields = [
        ("id", str(release.get("id", ""))),
        ("title", release.get("title") or ""),
        ("author", group_names(release)),
        ("date", str(release.get("release_year") or "")),
        ("party", party_field(release)),
        ("category", category(release)),
        ("rating", rating),
        ("download", ";".join(urls)),
    ]
    # A field with nothing after its colon carries no information, so leave it
    # out rather than write it empty.
    fields = [(key, clean(val)) for key, val in fields]
    return "\t".join(f"{key}:{val}" for key, val in fields if val)


def category_stats(counts: Counter) -> None:
    """Print a line count per category, most-released first."""
    width = max((len(c) for c in counts), default=0)
    for name, count in counts.most_common():
        print(f"  {name:<{width}}  {count}", file=sys.stderr)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--min", type=int, default=1, help="lowest release id")
    parser.add_argument(
        "--max",
        type=int,
        default=None,
        help="highest release id (default: go on until CSDb has no more)",
    )
    parser.add_argument(
        "--type", type=int, default=1, help="releasetypeid (1 = C64 release)"
    )
    parser.add_argument("--out", type=Path, default=Path("csdb.txt"))
    parser.add_argument("--cache-dir", type=Path, default=Path(".csdb_cache"))
    parser.add_argument(
        "--only-with-url",
        action="store_true",
        help="skip releases that have no download link",
    )
    parser.add_argument(
        "--no-ratings",
        action="store_true",
        help="skip the toplist pages; write no rating fields",
    )
    parser.add_argument(
        "--refresh-ratings",
        action="store_true",
        help="refetch the toplist pages instead of reading the cached ones",
    )
    args = parser.parse_args()

    args.cache_dir.mkdir(parents=True, exist_ok=True)

    ratings: dict[int, str] = {}
    if not args.no_ratings:
        ratings = load_ratings(args.cache_dir, args.refresh_ratings)

    releases = load_releases(args.cache_dir, args.min, args.max, args.type)

    rows = ["# CSDb database", f"# Platform:{PLATFORM}"]
    categories: Counter = Counter()
    skipped_download = 0  # had links, but none demarc could use
    skipped_no_link = 0  # had none to begin with (--only-with-url)
    rated = 0  # lines carrying a rating field
    for rid in sorted(releases):
        rel = releases[rid]
        links = rel.get("download_links") or []
        urls = downloads(rel)
        if links and not urls:
            skipped_download += 1
            continue
        if args.only_with_url and not urls:
            skipped_no_link += 1
            continue
        rating = ratings.get(rid, "")
        rated += bool(rating)
        rows.append(to_row(rel, urls, rating))
        categories[category(rel)] += 1

    if len(rows) == 1:
        raise SystemExit(
            f"error: no releases to export -- {skipped_download} had no usable "
            f"download and {skipped_no_link} had none at all.  If both counts "
            f"are zero the cache is empty; check --min/--max and --cache-dir."
        )

    # Write beside the target and rename, so a run that reads a truncated cache
    # cannot replace a good export with a short one for `just export` to gzip
    # over the previous download.
    tmp_path = args.out.with_name(args.out.name + ".tmp")
    tmp_path.write_text("\n".join(rows) + "\n", encoding="utf-8")
    tmp_path.replace(args.out)

    category_stats(categories)
    print(
        f"\nWrote {len(rows) - 1} releases to {args.out}, {rated} of them rated "
        f"(skipped {skipped_download} without a usable download"
        + (f", {skipped_no_link} with no download at all" if args.only_with_url else "")
        + ")",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
