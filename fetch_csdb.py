#!/usr/bin/env python3
"""Download C64 release metadata from CSDb and write a tab-separated csdb.txt.

The CSDb "releases_extract" API returns releases in id ranges of at most 1000.
We walk the requested id range in 1000-wide blocks, caching each block's raw
JSON on disk so re-runs don't refetch data we already have, then emit a
csdb.txt in the named-field db format read by src/files.rs — a
`# Platform:C64` header followed by one tab-separated line per release:

    id:1<TAB>title:Zentro 4<TAB>author:Zenith<TAB>date:1992<TAB>party:The
    Party 1992<TAB>category:Demo<TAB>tags:<TAB>download:http://...

Usage:
    ./fetch_csdb.py --min 1 --max 263500
"""

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

API = "https://csdb.dk/misc/releases_extract.php"
BLOCK = 1000  # API requires releasemaxid - releaseminid <= 1000
PLATFORM = "C64"  # header platform, prefixed to every entry's category


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


def load_block(cache_dir: Path, min_id: int, max_id: int, release_type: int) -> list:
    """Return a block's releases from cache, fetching and caching on a miss."""
    cache_file = cache_dir / f"block_{min_id}_{max_id}.json"
    if cache_file.exists():
        releases = json.loads(cache_file.read_text())
        print(f"cached  {min_id}-{max_id}: {len(releases)} releases")
        return releases
    print(f"fetch   {min_id}-{max_id} ...", end="", flush=True)
    releases = fetch_block(min_id, max_id, release_type)
    cache_file.write_text(json.dumps(releases, ensure_ascii=False))
    print(f" {len(releases)} releases")
    time.sleep(0.5)  # be gentle on the server
    return releases


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


def category(release: dict) -> tuple[str, str | None]:
    """Split a CSDb release type into (category, per-line platform).

    Types read "C64 Demo", "C64 One-File Demo" and so on; the platform lives in
    the `# Platform:C64` header, so it's stripped from the category here. Types
    naming something else ("C128 Release", "BBS Software") get an empty
    `platform:` field, which overrides the header so they aren't mislabelled.
    """
    demo_type = release.get("type", "")
    rest = demo_type.removeprefix(PLATFORM).lstrip()
    if rest != demo_type:
        return rest or "Release", None
    return demo_type, ""


def to_row(release: dict) -> str:
    """Format one release as a named-field, tab-separated csdb.txt line."""
    demo_type, platform = category(release)
    fields = {
        "id": str(release.get("id", "")),
        "title": release.get("title", ""),
        "author": group_names(release),
        "date": str(release.get("release_year") or ""),
        "party": release.get("event_name") or "",
        "category": demo_type,
        "tags": "",
        "download": ";".join(release.get("download_links") or []),
    }
    if platform is not None:
        fields["platform"] = platform
    # Guard against stray tabs/newlines in the source data; a value may hold
    # ':' freely, since only the first one separates key from value.
    def clean(val: str) -> str:
        return val.replace("\t", " ").replace("\n", " ").strip()

    return "\t".join(key + ":" + clean(val) for key, val in fields.items())


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--min", type=int, default=1, help="lowest release id")
    parser.add_argument("--max", type=int, default=263500, help="highest release id")
    parser.add_argument("--type", type=int, default=1, help="releasetypeid (1 = C64 release)")
    parser.add_argument("--out", type=Path, default=Path("csdb.txt"))
    parser.add_argument("--cache-dir", type=Path, default=Path(".csdb_cache"))
    parser.add_argument(
        "--only-with-url",
        action="store_true",
        help="skip releases that have no download link",
    )
    args = parser.parse_args()

    args.cache_dir.mkdir(parents=True, exist_ok=True)

    releases: dict[int, dict] = {}
    for start in range(args.min, args.max + 1, BLOCK):
        end = min(start + BLOCK, args.max)
        for rel in load_block(args.cache_dir, start, end, args.type):
            rid = rel.get("id")
            if rid is not None:
                releases[rid] = rel  # dedupe overlapping block boundaries

    rows = [f"# Platform:{PLATFORM}"]
    for rid in sorted(releases):
        rel = releases[rid]
        if args.only_with_url and not (rel.get("download_links") or []):
            continue
        rows.append(to_row(rel))

    args.out.write_text("\n".join(rows) + "\n", encoding="utf-8")
    print(f"\nwrote {len(rows) - 1} releases to {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
