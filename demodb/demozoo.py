#!/usr/bin/env python3
"""demozoo — populate a SQLite database from a Demozoo PostgreSQL dump and
export all releases in the named-field db format read by src/files.rs, the same
one bitworld.txt and csdb.txt use:

    id:1<TAB>title:Zentro 4<TAB>author:Zenith<TAB>date:1992-12-27<TAB>party:The
    Party 1992;Amiga Demo;3<TAB>platform:Amiga<TAB>category:Demo<TAB>download:http://...

A field with nothing after its colon says nothing, so it is left out of the
line altogether -- an absent field and an empty one mean the same thing, and
demarc skips both when it reads the file back.

Demozoo covers every platform, so unlike the single-platform bitworld.txt and
csdb.txt there is no `# Platform:` header — each line names its own platform.
Only the platforms in PLATFORM_WHITELIST are exported; releases carrying a
blacklisted tag (see TAG_BLACKLIST) or a blacklisted production type (see
CATEGORY_BLACKLIST) are dropped, as are downloads with a blacklisted extension
(see DOWNLOAD_BLACKLIST) or a url demarc cannot read back (see url_usable) --
and with them any release left without a download.
Graphics and music for which Demozoo records no platform at all are still
exported, with no platform field at all (see PLATFORMLESS_SUPERTYPES).

`party` is the party name, and where Demozoo records the release as a
competition placing it carries the compo and the place in it too, separated by
';' ("party:Evoke 1984;4K Intro;2" is the runner-up in Evoke 1984's 4K Intro
compo).  An entry with no ranking leaves that part empty; a release that Demozoo
lists among a party's releases without any compo is the bare party name, with no
';' at all (see party_field).

A `download` is either a whole url or, for the file archives Demozoo records as
a link_class plus a parameter, that pair as `SceneOrgFile:/parties/2006/x.zip`.
Which mirror such a link resolves to is demarc's to decide, not ours -- see the
URL resolution section below.  Alongside it a `dlname` field carries each
download's own label from Demozoo ("HD Version", ...), semicolon-separated and
in step with `download` so the nth name goes with the nth url; unlabelled links
leave their slot empty, and a release whose links are all unlabelled gets no
`dlname` field.

A release that Demozoo links to a pouet.net prod also gets a
`pouet:<cncds>,<thumbs>,<rank>,<won>,<nominated>` field, the last two being
space-separated award category ids ("pouet:9,398,19,1 5 7,8 55").  Where those
numbers come from is a choice:

  * by default only the per-platform toplists (top 64 each) are read, so only
    the handful of releases that made a list carry the field.  The pages are
    fetched at startup and cached under .pouet_cache.  A toplist row only
    carries the votes and the CDCs, so rank and awards come out empty.
  * with --pouet-prods the toplists are not read at all; instead every
    exported release that has a pouet link is looked up in pouet.py, which
    answers out of pouet's offline data dump (pouetdatadump-prods-*.json) if
    one is there and otherwise fetches and caches the prod's JSON API page.
    That covers every linked release rather than 64 per platform.  With a dump
    it costs nothing; without one it is a request per release -- tens of
    thousands on a cold cache, paced by --pouet-delay and, for a trial run,
    capped by --pouet-limit.

The two are alternatives, not a fallback: one source or the other.
--refresh-pouet refetches whichever is in use and --no-pouet skips the step.

Usage:
    python demozoo.py --sql demozoo-export.sql --db demozoo.sqlite \
        --out demozoo.txt

By default the database is rebuilt from the SQL file. Pass --skip-load to reuse
an existing SQLite database and only regenerate the export (much faster).
"""

import argparse
from dataclasses import dataclass, field
import ipaddress
import os
import re
import sqlite3
import sys
import time
from fnmatch import fnmatchcase
from urllib.parse import unquote
from urllib.request import Request, urlopen

import pouet

# ---------------------------------------------------------------------------
# Which tables (and which of their columns) we pull out of the pg_dump.
# We only load what the export actually needs.
# ---------------------------------------------------------------------------
TABLES = {
    "productions_production": [
        "id",
        "title",
        "release_date_date",
        "release_date_precision",
        "supertype",
    ],
    "productions_production_author_nicks": ["production_id", "nick_id"],
    "productions_production_author_affiliation_nicks": ["production_id", "nick_id"],
    "demoscene_nick": ["id", "releaser_id", "name"],
    "demoscene_releaser": ["id", "is_group"],
    "productions_production_types": ["production_id", "productiontype_id"],
    "productions_productiontype": ["id", "name"],
    "productions_production_platforms": ["production_id", "platform_id"],
    "platforms_platform": ["id", "name"],
    "parties_competitionplacing": ["competition_id", "production_id", "ranking"],
    "parties_competition": ["id", "party_id", "name"],
    "parties_party_releases": ["party_id", "production_id"],
    "parties_party": ["id", "name"],
    "productions_productionlink": [
        "production_id",
        "link_class",
        "parameter",
        "is_download_link",
        "description",
    ],
    "taggit_tag": ["id", "name"],
    "taggit_taggeditem": ["tag_id", "object_id", "content_type_id"],
}

# django_content_type id of the 'production' model (productions app).
# Values load from the dump as TEXT, so keep this a string for comparison.
PRODUCTION_CONTENT_TYPE = "12"

# ---------------------------------------------------------------------------
# Demozoo spells its platforms out in full ("Commodore 64"); demarc names the
# same machines the way bitworld.txt and csdb.txt do, so that one filter
# (`-I platform:C64`) picks the same platform out of any of the three dbs.
# Platforms not listed here are exported under their Demozoo name.
# ---------------------------------------------------------------------------
PLATFORM_NAMES = {
    "Commodore 64": "C64",
    "Commodore 64-DTV": "C64 DTV",
    "Commodore 128": "C128",
    "Commodore 16/Plus 4": "C16",
    "Commodore VIC-20": "VIC-20",
    "Amiga OCS/ECS": "Amiga",
    "Amiga AGA": "Amiga AGA",
    "Amiga PPC/RTG": "Amiga PPC",
    "Atari ST/E": "Atari ST",
    "Atari 8 bit": "Atari XL",
    "Atari 2600 Video Computer System (VCS)": "Atari 2600",
    "Sega Megadrive/Genesis": "Megadrive",
    "Nintendo SNES/Super FamiCom": "SNES",
    "Nintendo Game Boy (GB)": "Gameboy",
    "Nintendo Game Boy Color (GBC)": "Gameboy Color",
    "Nintendo Game Boy Advance (GBA)": "GBA",
    "Sony Playstation 1 (PSX)": "PlayStation",
    "Sony Playstation Portable (PSP)": "PSP",
    "TIC-80": "Tic-80",
    "PICO-8": "Pico8",
}

POUET_IDS = {
    "C64": 76,
    "Amiga": 73,
    "Amiga AGA": 71,
    "Atari ST": 70,
    "Atari STe": 72,  # NOTE: joined with ST for demarc
    "PlayStation": 75,
    "Gameboy": 81,
    "Gameboy Color": 86,
    "GBA": 85,
    "Megadrive": 89,
    "Atari 2600": 117,
    "Amstrad CPC": 78,
    "Atari XL": 109,
    "MS-DOS": 67,
    "MS-DOS/Gus": 69,
}

POUET_TOPLIST_URL = "https://www.pouet.net/toplist.php?type=&platform={id}&limit=64"

# Toplist pages are fetched once and kept here; delete the directory (or pass
# --refresh-pouet) to pick up newer vote counts.  A cached copy also means a
# rebuild works offline and does not hammer pouet.net once per platform.
POUET_CACHE_DIR = ".pouet_cache"


@dataclass
class PouetData:
    """The pouet numbers for one release, as the `pouet:` field spells them.

    `rank` is the prod's alltime-top position (0 when it has none) and
    `winners`/`nominees` are the award category ids it won and was nominated
    for.  Only --pouet-prods knows any of those three: a toplist row carries
    the votes and the CDCs and nothing else, so in toplist mode they stay at
    their empty defaults."""

    id: int
    thumbs: int
    cncd_count: int
    rank: int = 0
    winners: list[int] = field(default_factory=list)
    nominees: list[int] = field(default_factory=list)

    def field_value(self) -> str:
        """`<cncds>,<thumbs>,<rank>,<won ids>,<nominated ids>`, the last two
        space-separated ('9,398,19,1 5 7,8 55').  A prod with no rank and no
        awards ends in '0,,' -- the field is fixed-width in commas, so demarc
        can split it without counting."""
        return ",".join(
            [
                str(self.cncd_count),
                str(self.thumbs),
                str(self.rank),
                " ".join(str(i) for i in self.winners),
                " ".join(str(i) for i in self.nominees),
            ]
        )


_PROD_ID_RE = re.compile(r"prod\.php\?which=(\d+)")
_LEADING_INT_RE = re.compile(r"\d+")


def _leading_int(text):
    """First integer in `text` ('18 CDCs' -> 18), or 0 if there is none."""
    if not text:
        return 0
    m = _LEADING_INT_RE.search(text)
    return int(m.group()) if m else 0


def fetch_page(url):
    """GET `url` as text.  Pouet 403s the default urllib agent, so pose as a
    browser the way bitworld_scrape.py does."""
    req = Request(url, headers={"User-Agent": "Mozilla/5.0"})
    with urlopen(req, timeout=60) as resp:
        return resp.read().decode("utf-8", errors="replace")


def parse_toplist(html: str) -> list[PouetData]:
    """Pull the release rows out of a Pouet toplist page.

    The list is a single <ul class='boxlist boxlisttable'>, one <li> per
    release, holding the three things we want:

        <span class='prod'><a href='prod.php?which=25778'>Starstruck</a></span>
        <div class='cdcstack' title='18 CDCs'>...</div>
        <span class='toplist rulez'>576</span>

    A release with no CDCs simply has no cdcstack div, which reads as 0.  Rows
    without a prod link (there are none today, but the page also carries award
    and group links) are skipped rather than guessed at.
    """
    from bs4 import BeautifulSoup

    soup = BeautifulSoup(html, "html.parser")
    ul = soup.find("ul", class_="boxlisttable")
    if ul is None:
        return []

    out = []
    for li in ul.find_all("li", recursive=False):
        link = li.select_one("span.prod a[href*='prod.php?which=']")
        if link is None:
            continue
        m = _PROD_ID_RE.search(link["href"])
        if not m:
            continue
        cdc = li.find("div", class_="cdcstack")
        rulez = li.find("span", class_="rulez")
        out.append(
            PouetData(
                id=int(m.group(1)),
                thumbs=_leading_int(rulez.get_text() if rulez else ""),
                cncd_count=_leading_int(cdc.get("title") if cdc else ""),
            )
        )
    return out


def toplist_html(platform_id, cache_dir=POUET_CACHE_DIR, refresh=False):
    """The toplist page for one platform, from the disk cache if we have it."""
    path = os.path.join(cache_dir, f"toplist-{platform_id}.html")
    if not refresh and os.path.exists(path):
        with open(path, encoding="utf-8") as f:
            return f.read()
    html = fetch_page(POUET_TOPLIST_URL.format(id=platform_id))
    os.makedirs(cache_dir, exist_ok=True)
    # Same write-then-rename as everywhere else here: a half-written page left
    # by an interrupted fetch would be cached as if it were the real thing.
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        f.write(html)
    os.replace(tmp, path)
    return html


def populate_pouetdata(
    platform_id: int, cache_dir=POUET_CACHE_DIR, refresh=False
) -> list[PouetData]:
    """Fetch (or read back) the Pouet toplist for one platform id (POUET_IDS)."""
    return parse_toplist(toplist_html(platform_id, cache_dir, refresh))


def load_pouetdata(cache_dir=POUET_CACHE_DIR, refresh=False):
    """{pouet prod id: PouetData} over every platform in POUET_IDS.

    A prod that targets two of these platforms (an ST/STe release, say) is
    listed once per platform with the same votes, so the first row wins.  A
    platform whose page cannot be fetched is warned about and skipped rather
    than failing the whole export -- the pouet field is an extra, not the point
    of the file.
    """
    data = {}
    for name, platform_id in POUET_IDS.items():
        try:
            rows = populate_pouetdata(platform_id, cache_dir, refresh)
        except OSError as e:
            print(f"  WARNING: no pouet toplist for {name}: {e}", file=sys.stderr)
            continue
        for row in rows:
            data.setdefault(row.id, row)
    print(f"Pouet toplists: {len(data)} releases", file=sys.stderr)
    return data


# One request per release is a lot of requests, so leave a gap between them.
# Only real fetches are paced; a warm .pouetcache runs at full speed.
POUET_PROD_DELAY = 0.5


def pouet_prod_lookup(
    cache_dir=pouet.CACHE_DIR, refresh=False, delay=POUET_PROD_DELAY, limit=0
):
    """The --pouet-prods alternative to the toplists: a `pouet_id -> PouetData`
    lookup that asks pouet's per-prod JSON API (pouet.py) instead.

    Fetching is lazy on purpose.  export() only asks about releases it is
    actually writing out, so the off-whitelist and no-download majority never
    costs a request -- which matters when the linked prods number six figures.
    A prod that 404s or cannot be fetched is counted and skipped: this field is
    an extra, not the point of the file.

    `limit`, if set, caps how many prods this run will *fetch* (--pouet-limit).
    Once it is reached the export still finishes, but the releases past it get
    no pouet field rather than a request -- which is what makes a trial run of
    a few dozen possible without waiting out all fifty thousand.  Prods that
    cost no request -- the ones in pouet's data dump, and the ones already in
    the cache -- never count against the limit, so with a dump in place the
    limit and the delay simply never come into play.
    """
    stats = {"fetched": 0, "failed": 0, "skipped": 0}

    def lookup(pouet_id):
        if pouet_id is None:
            return None
        cached = not refresh and pouet.is_cached(pouet_id, cache_dir)
        if not cached and limit and stats["fetched"] >= limit:
            stats["skipped"] += 1
            if stats["skipped"] == 1:
                print(
                    f"  --pouet-limit {limit} reached; the rest of the "
                    f"export goes without pouet data",
                    file=sys.stderr,
                )
            return None
        try:
            prod = pouet.get_prod(pouet_id, cache_dir, refresh)
        except OSError as e:
            stats["failed"] += 1
            if stats["failed"] <= 10:
                print(f"  WARNING: no pouet prod {pouet_id}: {e}", file=sys.stderr)
            return None
        if not cached:
            stats["fetched"] += 1
            if stats["fetched"] % 100 == 0:
                print(
                    f"  fetched {stats['fetched']} pouet prod pages "
                    f"({stats['failed']} failed)",
                    file=sys.stderr,
                )
            if delay:
                time.sleep(delay)
        if prod is None:
            return None
        return PouetData(
            id=prod.id,
            thumbs=prod.rulez,
            cncd_count=prod.cdc,
            rank=prod.rank,
            winners=prod.winner_ids,
            nominees=prod.nominee_ids,
        )

    return lookup


# ---------------------------------------------------------------------------
# What gets exported at all.
#
# PLATFORM_WHITELIST is matched (case-sensitively, as a glob) against the
# *demarc* platform name, i.e. the right-hand side of PLATFORM_NAMES — so
# "Amiga*" covers Amiga, Amiga AGA and Amiga PPC in one pattern.  Platforms
# that match nothing are stripped from the platform field, and a release left
# without any platform is not exported — except for the platformless graphics
# and music described at PLATFORMLESS_SUPERTYPES.
#
# DOWNLOAD_BLACKLIST is matched against the lower-cased URL with any query
# string removed, so the patterns are effectively extensions.  ('#' is left
# alone: it is a literal character in plenty of these unencoded file names,
# e.g. modland's ".../XTD/## crimple ##.mod".)
# demarc runs what it downloads; a bare .mod or a video capture is not a
# release you can run.  Such urls are dropped, and a release whose downloads
# are *all* dropped goes with them.
# ---------------------------------------------------------------------------
PLATFORM_WHITELIST = [
    "Amiga AGA",
    "Amiga",
    "Atari ST*",
    "Atari XL",
    "Atari 2600",
    "C64",
    "C16",
    "Megadrive",
    "SNES",
    "Gameboy*",
    "GBA",
    "PlayStation",
    "Tic-80",
    "ZX Spectrum",
    "Amstrad CPC",
    "Neo Geo",
    "MS-Dos",
    "Windows",
    "Pico*",
    "PSP",
]

# Demozoo only really insists on a platform for executable prods; for graphics
# and music it is optional, and some 116k such entries carry no platform row at
# all (as opposed to one we filtered out).  Dropping everything platformless
# would therefore throw away most of Demozoo's pictures and tunes, so entries of
# these supertypes are kept when they have a usable download, and exported with
# no `platform:` field — a `-I platform:...` filter will not match them.
PLATFORMLESS_SUPERTYPES = {"graphics", "music"}

# Demozoo tags that mean "there is nothing here demarc can run": a release
# carrying any of these is dropped whatever its downloads look like.
TAG_BLACKLIST = {"no-binary", "no-binaries"}

# Demozoo production types demarc has no business launching: a game or a tool
# is not a release to watch, and a video is a recording of one rather than the
# thing itself.  Matched against the type names exactly as Demozoo spells them
# (the `category:` field); a release carrying any of them is dropped, even when
# it also carries a type we do want.
CATEGORY_BLACKLIST = {"Game", "Tool", "Video"}

DOWNLOAD_BLACKLIST = [
    "*.php",
    "*.ogg",
    "*.mp4",
    "*.mpg",
    "*.avi",
    "*.wmv",
    "*.mkv",
    "*.pdf",
    "*.wav",
    "*.ogg",
    "*.4q",
]


def platform_allowed(name):
    return any(fnmatchcase(name, pat) for pat in PLATFORM_WHITELIST)


def download_allowed(url):
    path = url.split("?", 1)[0].lower()
    return not any(fnmatchcase(path, pat) for pat in DOWNLOAD_BLACKLIST)


# ---------------------------------------------------------------------------
# URL parsability, mirroring what demarc does with the field we write.
#
# collect_db_text() in demarc's src/files.rs runs every `download:` url through
# the Rust `url` crate and warns-and-drops the ones that do not parse; a line
# left with no url at all is skipped entirely.  Exporting such a url therefore
# only produces a warning at load time, and exporting a release whose urls all
# fail produces a line demarc silently throws away -- so we drop both here.
#
# `url::Url::parse` implements the WHATWG URL Standard, and url_parsable() is a
# reimplementation of the parts of it that can fail: scheme, authority, host and
# port.  It is deliberately not a full parser -- what it does not do is IDNA, so
# a non-ASCII host that idna would reject is accepted here (Demozoo has none).
# Everything after the authority (path, query, fragment) is percent-encoded
# rather than rejected, so it never decides parsability and is not looked at.
#
# In practice nearly every url that fails is a Demozoo BaseUrl parameter that is
# not a url at all -- 'fuckit', '_-_minako.zip', 'attach=29942'.
# ---------------------------------------------------------------------------

# scheme, and the ':' that ends it.  Without one there is nothing to resolve a
# relative url against, which is `RelativeUrlWithoutBase`, the failure Demozoo
# actually produces.
_URL_SCHEME_RE = re.compile(r"[A-Za-z][A-Za-z0-9+.\-]*:")

# The standard's "special" schemes: they always have an authority, so an empty
# host is an error -- except for file:, which is the one that may have none.
_SPECIAL_SCHEMES = {"http", "https", "ws", "wss", "ftp", "file"}

# C0 controls and space are stripped from both ends of a url before parsing,
# and tab/LF/CR are removed from anywhere in it.
_C0_OR_SPACE = "".join(chr(c) for c in range(0x21))
_TAB_OR_NEWLINE = {0x09: None, 0x0A: None, 0x0D: None}

# "Forbidden host code point": these cannot appear in a host.  (Several of them
# would end the host anyway; keeping the whole set makes the check match the
# standard's list rather than an ad-hoc subset.)
_FORBIDDEN_HOST = set("\x00\t\n\r #/:<>?@[\\]^|\x7f")


def _ipv4_number(part):
    """The value of one dotted part, or None if it is not a number at all.

    Bases follow C: `0x` is hex, a leading `0` is octal, anything else decimal.
    """
    if part[:2].lower() == "0x":
        digits, base = part[2:], 16
    elif len(part) > 1 and part[0] == "0":
        digits, base = part[1:], 8
    else:
        digits, base = part, 10
    if digits == "":
        return 0  # bare '0' / '0x'
    try:
        return int(digits, base)
    except ValueError:
        return None


def _ipv4_ok(parts):
    """Whether a host that ends in a number is a valid IPv4 address.

    The last part absorbs whatever room the earlier ones leave -- `1.2.771` is
    1.2.3.3 -- so only a value too big for the remaining bytes is an error.
    """
    if len(parts) > 4:
        return False
    numbers = [_ipv4_number(p) for p in parts]
    if any(n is None for n in numbers):
        return False
    if any(n > 255 for n in numbers[:-1]):
        return False
    return numbers[-1] < 256 ** (5 - len(parts))


def _domain_ok(host):
    """Whether a special scheme's host is a usable domain or IPv4 address."""
    try:
        # A domain is percent-decoded before it is checked, so '%41' is 'A'.
        host = unquote(host, errors="strict")
    except (UnicodeDecodeError, ValueError):
        return False
    if not host or any(c in _FORBIDDEN_HOST or c == "%" or ord(c) < 0x20 for c in host):
        return False
    parts = host.split(".")
    if len(parts) > 1 and parts[-1] == "":
        parts.pop()  # a trailing dot is a fully qualified name, not an error
    # A host whose last part reads as a number is an IPv4 address, and is then
    # held to IPv4's rules rather than a domain's.
    last = parts[-1] if parts else ""
    if last.isdigit() or (
        last[:2].lower() == "0x"
        and all(c in "0123456789abcdefABCDEF" for c in last[2:])
    ):
        return _ipv4_ok(parts)
    return True


def _host_and_port_ok(authority, scheme):
    """Whether the `host[:port]` half of an authority parses."""
    if authority.startswith("["):
        # An IPv6 literal, which has to be closed before the port.
        end = authority.find("]")
        if end < 0:
            return False
        host, rest = authority[: end + 1], authority[end + 1 :]
        # The standard's IPv6 parser knows nothing of scope ids, which
        # ipaddress does accept -- so `[fe80::1%25eth0]` is an error here.
        if "%" in host:
            return False
        try:
            ipaddress.IPv6Address(host[1:-1])
        except ValueError:
            return False
    else:
        host, _, rest = authority.partition(":")
        rest = rest and ":" + rest
        if not host:
            # Only file: (and the non-special schemes) may go without a host.
            if scheme in _SPECIAL_SCHEMES and scheme != "file":
                return False
        elif scheme in _SPECIAL_SCHEMES:
            if not _domain_ok(host):
                return False
        elif any(c in _FORBIDDEN_HOST or ord(c) < 0x20 for c in host):
            return False
    if not rest:
        return True
    port = rest[1:]  # rest starts with the ':' that introduces the port
    # An empty port is allowed and simply means the scheme's default.
    return port == "" or (port.isdigit() and int(port) <= 65535)


def url_parsable(url):
    """Whether demarc's `Url::parse` would accept `url` -- see the block above."""
    url = url.strip(_C0_OR_SPACE).translate(_TAB_OR_NEWLINE)
    m = _URL_SCHEME_RE.match(url)
    if not m:
        return False
    scheme = url[: m.end() - 1].lower()
    rest = url[m.end() :]
    if scheme in _SPECIAL_SCHEMES:
        # A special scheme always has an authority, however many (or few)
        # slashes were written: `http:example.com` is `http://example.com`.
        rest = rest.lstrip("/\\")
    elif rest.startswith("//"):
        rest = rest[2:]
    else:
        # Anything else is an opaque path (`mailto:`, `data:`) and is kept
        # as written, so there is nothing left that can fail.
        return True
    # The authority runs up to the first path/query/fragment delimiter, and the
    # last '@' in it ends the userinfo.
    end = len(rest)
    for c in "/?#" + ("\\" if scheme in _SPECIAL_SCHEMES else ""):
        i = rest.find(c)
        if i >= 0:
            end = min(end, i)
    authority = rest[:end]
    _, at, after_at = authority.rpartition("@")
    return _host_and_port_ok(after_at if at else authority, scheme)


def url_usable(url):
    """Whether demarc can get `url` back out of a `download:` field intact.

    It splits the field on ';' *before* parsing (files.rs, collect_db_text), so
    a url containing one never reaches the parser whole: it arrives as two or
    three pieces, of which the ones that do parse point at the wrong file and
    the ones that do not are warned about and dropped.  Such a url cannot be
    written to this field at all, however valid it is in itself, so it goes the
    same way an unparsable one does.  Demozoo has a few dozen, mostly
    `?action=dlattach;attach=31364` forum links.
    """
    return ";" not in url and url_parsable(url)


# ---------------------------------------------------------------------------
# URL resolution.  A Demozoo productionlink stores a link_class plus a
# parameter; the real URL is the parameter appended to a base url that depends
# on the class.  We do *not* resolve it here: the exported `download:` field
# keeps the pair as written,
#
#     download:SceneOrgFile:/parties/2006/assembly06/demo/bombman.zip
#
# and demarc turns that into a url when it downloads it (LINK_BASES in demarc's
# src/fetch.rs), where each class points at a list of mirrors that can be tried
# in turn.  A mirror that dies -- or a faster one appearing -- is then a demarc
# change rather than a regenerate of every db built from this dump.  The same
# goes for the fixups demarc applies to links that no longer work as recorded
# (URL_REWRITES there), which used to live here.
#
# BaseUrl is the exception: its parameter already is a whole url, so it is
# exported as it stands.
#
# One productionlink table holds both lists a Demozoo prod page shows: the
# download links and the external links (Pouet/csdb/Youtube/Bandcamp pages).
# demarc fetches the exported url and tries to *run* it, so only the former
# belong in `download:` -- a prod whose only links are info pages exports an
# empty download rather than a web page demarc cannot load.  Hence only file
# classes are listed here; everything else resolves to None and is dropped.
#
# The order is the order in which classes are preferred when a release has
# several links: the dedicated file archives first (matches bitworld, which
# points at files); BaseUrl last, since it is the catch-all class and its
# is_download_link flag is the only thing telling a file apart from a home page.
#
# Every class here must be one demarc knows, so keep this list and LINK_BASES
# in step.
# ---------------------------------------------------------------------------
URL_PRIORITY = [
    "AmigascneFile",
    "SceneOrgFile",
    "ModlandFile",
    "FujiologyFile",
    "UntergrundFile",
    "PaduaOrgFile",
    "Defacto2File",
    "ModarchiveModule",
    "SixteenColorsPack",
    "BaseUrl",
]
URL_RANK = {cls: i for i, cls in enumerate(URL_PRIORITY)}


def resolve_url(link_class, parameter):
    """The `download:` value for one productionlink, or None for a link class
    demarc cannot resolve."""
    if link_class not in URL_RANK:
        return None
    parameter = parameter.strip()
    if not parameter:
        return None
    if link_class == "BaseUrl":
        return parameter
    return f"{link_class}:{parameter}"


# ---------------------------------------------------------------------------
# COPY-format parsing
# ---------------------------------------------------------------------------
_UNESCAPE = {
    "\\": "\\",
    "b": "\b",
    "f": "\f",
    "n": "\n",
    "r": "\r",
    "t": "\t",
    "v": "\v",
}


def unescape(value):
    """Un-escape a single field from a PostgreSQL COPY (text format) line."""
    if value == "\\N":
        return None
    if "\\" not in value:
        return value
    out = []
    i = 0
    n = len(value)
    while i < n:
        c = value[i]
        if c == "\\" and i + 1 < n:
            nxt = value[i + 1]
            if nxt in _UNESCAPE:
                out.append(_UNESCAPE[nxt])
                i += 2
                continue
        out.append(c)
        i += 1
    return "".join(out)


def load_database(sql_path, db_path):
    # Build into a scratch file and move it into place only once the load has
    # committed.  journal_mode=OFF below trades the rollback journal for speed,
    # so a load interrupted part-way (Ctrl-C, OOM) cannot be rolled back: were
    # we writing db_path directly it would be left with the previous build
    # dropped, half the tables empty and its freelist inconsistent -- and, being
    # a perfectly openable file, every later --skip-load run would read it as if
    # it were good and export nothing.  Renaming makes the db valid or absent.
    tmp_path = db_path + ".tmp"
    if os.path.exists(tmp_path):
        os.remove(tmp_path)
    conn = sqlite3.connect(tmp_path)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")
    cur = conn.cursor()

    for table, cols in TABLES.items():
        cur.execute(f"DROP TABLE IF EXISTS {table}")
        cur.execute(f"CREATE TABLE {table} ({', '.join(cols)})")

    remaining = dict(TABLES)  # tables we still need to find in the dump
    current = None  # (table, [source col indices], cols)
    batch = []
    inserted = {t: 0 for t in TABLES}

    def flush():
        if current and batch:
            table = current[0]
            placeholders = ", ".join("?" * len(current[2]))
            cur.executemany(f"INSERT INTO {table} VALUES ({placeholders})", batch)
            inserted[table] += len(batch)
        batch.clear()

    t0 = time.time()
    with open(sql_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if current is None:
                if line.startswith("COPY public.") and remaining:
                    # Is this one of the tables we care about?
                    name = line[len("COPY public.") : line.index(" (")]
                    if name in remaining:
                        all_cols = line[line.index("(") + 1 : line.index(")")].split(
                            ", "
                        )
                        wanted = TABLES[name]
                        idx = [all_cols.index(c) for c in wanted]
                        current = (name, idx, wanted)
                continue

            # Inside a COPY block for a wanted table.
            if line.startswith("\\."):
                flush()
                del remaining[current[0]]
                current = None
                if not remaining:
                    break
                continue

            raw = line.rstrip("\n").split("\t")
            idx = current[1]
            batch.append(tuple(unescape(raw[i]) for i in idx))
            if len(batch) >= 20000:
                flush()

    flush()
    conn.commit()

    dt = time.time() - t0
    print(f"Loaded dump in {dt:.1f}s:", file=sys.stderr)
    for t, n in inserted.items():
        print(f"  {n:>8} {t}", file=sys.stderr)
    if remaining:
        print(
            f"  WARNING: tables not found in dump: {', '.join(remaining)}",
            file=sys.stderr,
        )

    build_indexes(cur)
    conn.commit()
    conn.close()
    os.replace(tmp_path, db_path)
    return sqlite3.connect(db_path)


def build_indexes(cur):
    for stmt in [
        "CREATE INDEX ix_pan_prod ON productions_production_author_nicks(production_id)",
        "CREATE INDEX ix_paan_prod ON productions_production_author_affiliation_nicks(production_id)",
        "CREATE INDEX ix_nick_id ON demoscene_nick(id)",
        "CREATE INDEX ix_rel_id ON demoscene_releaser(id)",
        "CREATE INDEX ix_ptypes_prod ON productions_production_types(production_id)",
        "CREATE INDEX ix_ptype_id ON productions_productiontype(id)",
        "CREATE INDEX ix_pplat_prod ON productions_production_platforms(production_id)",
        "CREATE INDEX ix_plat_id ON platforms_platform(id)",
        "CREATE INDEX ix_cp_prod ON parties_competitionplacing(production_id)",
        "CREATE INDEX ix_comp_id ON parties_competition(id)",
        "CREATE INDEX ix_pr_prod ON parties_party_releases(production_id)",
        "CREATE INDEX ix_party_id ON parties_party(id)",
        "CREATE INDEX ix_link_prod ON productions_productionlink(production_id)",
        "CREATE INDEX ix_ti_obj ON taggit_taggeditem(object_id, content_type_id)",
        "CREATE INDEX ix_tag_id ON taggit_tag(id)",
    ]:
        cur.execute(stmt)


# ---------------------------------------------------------------------------
# Export
# ---------------------------------------------------------------------------
def fmt_date(date, precision):
    """Render a Demozoo (date, precision) pair the way the db format wants."""
    if not date:
        return ""
    # date is ISO 'YYYY-MM-DD'
    if precision == "y":
        return date[:4]
    if precision == "m":
        return date[:7]
    return date  # 'd' or anything else -> full date


def clean(value):
    """Strip the separators the db format reserves out of a free-text value."""
    return value.replace("\t", " ").replace("\n", " ").replace("\r", " ")


def party_field(name, compo=None, ranking=None):
    """The `party:` value: the party name, and for a competition placing the
    compo and the place in it as well -- 'Evoke 1984;4K Intro;2'.

    ';' separates the three parts, so one is folded out of a name that carries
    it the way dlname does.  An unplaced entry (Demozoo leaves `ranking` empty
    for a compo whose results it has no order for) keeps the two separators and
    ends in ';', so the compo is still readable and the parts stay in fixed
    positions; a release that was in no compo at all is the bare party name,
    with no ';' in it.

    The place is Demozoo's `ranking` as it stands, which is free text rather
    than a number: most are '1', '2', ..., some 7k are a tie ('=10') or a
    non-result ('disqualified', 'not shown', 'preselected').  So read it as a
    string -- a consumer that wants an integer has to cope with those.
    """
    name = name.replace(";", ",")
    if compo is None:
        return name
    rank = (ranking or "").strip().replace(";", ",")
    return ";".join([name, compo.replace(";", ","), rank])


def scalar_maps(cur):
    """Pre-load small lookup tables into dicts for fast per-row assembly."""
    nick_name = {}
    nick_releaser = {}
    for nid, rel, name in cur.execute(
        "SELECT id, releaser_id, name FROM demoscene_nick"
    ):
        nick_name[nid] = name
        nick_releaser[nid] = rel
    is_group = {}
    for rid, g in cur.execute("SELECT id, is_group FROM demoscene_releaser"):
        is_group[rid] = g == "t"
    ptype_name = {}
    for pid, name in cur.execute("SELECT id, name FROM productions_productiontype"):
        ptype_name[pid] = name
    platform_name = {}  # whitelisted platforms only; others are absent
    for pid, name in cur.execute("SELECT id, name FROM platforms_platform"):
        name = PLATFORM_NAMES.get(name, name)
        if platform_allowed(name):
            platform_name[pid] = name
    party_name = {}
    for pid, name in cur.execute("SELECT id, name FROM parties_party"):
        party_name[pid] = name
    tag_name = {}
    for tid, name in cur.execute("SELECT id, name FROM taggit_tag"):
        tag_name[tid] = name
    return (
        nick_name,
        nick_releaser,
        is_group,
        ptype_name,
        platform_name,
        party_name,
        tag_name,
    )


def group_multimap(cur, table, key_col, val_col):
    """Return {key: [val, ...]} preserving insertion order."""
    d = {}
    for k, v in cur.execute(f"SELECT {key_col}, {val_col} FROM {table}"):
        d.setdefault(k, []).append(v)
    return d


def platform_stats(cur, exported=None):
    """Print a release count per platform, marking the whitelisted ones.

    Printed on every run because the whitelist is easy to get silently wrong:
    it is matched against the *demarc* name, so a PLATFORM_NAMES rename leaves
    its pattern matching nothing (`Atari 8*` once `Atari 8 bit` became
    `Atari XL`) and twelve thousand releases then disappear from the export
    with no error anywhere.  A pattern matching no platform at all is reported
    under the table; a whitelisted platform that exported nothing shows as a 0.

    Counts are platform rows rather than productions -- a release listed for
    two platforms is counted under both -- so they add up to more than the
    number of lines written.  `exported`, when given, is the
    {platform: lines written} tally export() kept as it wrote them.
    """
    stats = []
    for dz_name, count in cur.execute(
        "SELECT p.name, COUNT(*) FROM productions_production_platforms pp "
        "JOIN platforms_platform p ON p.id = pp.platform_id "
        "GROUP BY p.name"
    ):
        name = PLATFORM_NAMES.get(dz_name, dz_name)
        stats.append(
            (count, name, "" if dz_name == name else dz_name, platform_allowed(name))
        )
    if not stats:
        print("Platforms: none in the database", file=sys.stderr)
        return
    stats.sort(key=lambda s: (-s[0], s[1]))

    w1 = max(len(s[1]) for s in stats)
    w2 = max(len(s[2]) for s in stats)
    print(
        "Platforms ('*' is whitelisted; a release on two platforms is "
        "counted under both):",
        file=sys.stderr,
    )
    print(
        f"    {'demarc':<{w1}}  {'demozoo':<{w2}}  {'in dump':>8} {'exported':>8}",
        file=sys.stderr,
    )
    for count, name, dz_name, ok in stats:
        # Only the whitelisted ones can have been exported, so leave the
        # column blank for the rest rather than printing a meaningless 0.
        n_out = (
            f"{exported.get(name, 0):>8}" if ok and exported is not None else " " * 8
        )
        print(
            f"  {'*' if ok else ' '} {name:<{w1}}  {dz_name:<{w2}}  {count:>8} {n_out}",
            file=sys.stderr,
        )

    total = sum(s[0] for s in stats)
    listed = [s for s in stats if s[3]]
    listed_total = sum(s[0] for s in listed)
    print(
        f"  {len(stats)} platforms, {total} platform rows; "
        f"{len(listed)} whitelisted ({listed_total} rows, "
        f"{100 * listed_total / total:.1f}%)",
        file=sys.stderr,
    )
    dead = [
        pat
        for pat in PLATFORM_WHITELIST
        if not any(fnmatchcase(s[1], pat) for s in stats)
    ]
    if dead:
        print(
            f"  WARNING: whitelist patterns matching no platform: {', '.join(dead)}",
            file=sys.stderr,
        )


def export(conn, out_path, pouet_data=None, pouet_lookup=None):
    """Write the export.  `pouet_data` is the toplist map from load_pouetdata;
    `pouet_lookup` (--pouet-prods) replaces it with a per-prod callable that
    goes to pouet.net for one release at a time.  Exactly one of the two is in
    play -- there is no falling back from one to the other."""
    pouet_lookup = pouet_lookup or (pouet_data or {}).get
    cur = conn.cursor()
    (
        nick_name,
        nick_releaser,
        is_group,
        ptype_name,
        platform_name,
        party_name,
        tag_name,
    ) = scalar_maps(cur)

    author_nicks = group_multimap(
        cur, "productions_production_author_nicks", "production_id", "nick_id"
    )
    affil_nicks = group_multimap(
        cur,
        "productions_production_author_affiliation_nicks",
        "production_id",
        "nick_id",
    )
    prod_types = group_multimap(
        cur, "productions_production_types", "production_id", "productiontype_id"
    )
    prod_platforms = group_multimap(
        cur, "productions_production_platforms", "production_id", "platform_id"
    )

    # production -> party field (via competition placing, else party_releases).
    # A placing knows which compo it was and where it came, so those go in too,
    # semicolon-separated: 'Evoke 1984;4K Intro;2'.  A release only listed
    # among a party's releases has neither, and is just the party name.
    comp_party = {}  # competition_id -> (party_id, compo name)
    for cid, pid, name in cur.execute(
        "SELECT id, party_id, name FROM parties_competition"
    ):
        comp_party[cid] = (pid, name or "")
    prod_party = {}
    for comp_id, prod_id, ranking in cur.execute(
        "SELECT competition_id, production_id, ranking "
        "FROM parties_competitionplacing"
    ):
        party_id, compo = comp_party.get(comp_id, (None, ""))
        if party_id is None:
            continue
        name = party_name.get(party_id)
        if name is None:
            continue
        prod_party.setdefault(prod_id, party_field(name, compo, ranking))
    for party_id, prod_id in cur.execute(
        "SELECT party_id, production_id FROM parties_party_releases"
    ):
        name = party_name.get(party_id)
        if name is not None:
            prod_party.setdefault(prod_id, party_field(name))

    # production -> tags
    prod_tags = {}
    for tag_id, obj_id, ct in cur.execute(
        "SELECT tag_id, object_id, content_type_id FROM taggit_taggeditem"
    ):
        if ct == PRODUCTION_CONTENT_TYPE:
            prod_tags.setdefault(obj_id, []).append(tag_id)

    # production -> every url we can resolve, best first.  demarc downloads the
    # first entry (or, for a disk-image set, all of them), so ordering decides
    # what a release actually launches; the rest are fallbacks.
    prod_links = {}
    prod_pouet_id = {}  # production -> the pouet prod it is linked to
    for prod_id, link_class, parameter, is_dl, description in cur.execute(
        "SELECT production_id, link_class, parameter, is_download_link, "
        "description FROM productions_productionlink"
    ):
        # The external half of the table is pages *about* the release; the one
        # we keep is the Pouet link, whose parameter is the pouet prod id and
        # so is what ties a production to its toplist row.
        if link_class == "PouetProduction":
            if parameter and parameter.strip().isdigit():
                prod_pouet_id.setdefault(prod_id, int(parameter))
        # Skip the rest of the external links (csdb, Youtube, ...): they are not
        # the release itself.
        if is_dl != "t":
            continue
        rank = URL_RANK.get(link_class)
        if rank is None:
            continue
        url = resolve_url(link_class, parameter)
        if url:
            # A semicolon in a label would split the dlname field out of step
            # with download, so fold it away here.
            name = (description or "").strip().replace(";", ",")
            prod_links.setdefault(prod_id, []).append((rank, url, name))

    prod_urls = {}
    # dlname is the download link's own label ("HD Version", ...), kept in step
    # with the download field so the nth name goes with the nth url.
    prod_dlnames = {}
    # Had downloads, but none demarc could use: every one was either blacklisted
    # or a url it cannot parse.
    unusable = set()
    for prod_id, links in prod_links.items():
        seen = set()
        urls = []
        names = []
        for _, url, name in sorted(links, key=lambda l: l[0]):
            if url not in seen:
                seen.add(url)
                if download_allowed(url) and url_usable(url):
                    urls.append(url)
                    names.append(name)
        if urls:
            prod_urls[prod_id] = ";".join(urls)
            # Only carry names when at least one link is actually labelled,
            # so releases with no descriptions get an empty dlname field.
            if any(names):
                prod_dlnames[prod_id] = ";".join(names)
        else:
            unusable.add(prod_id)

    def join_names(names):
        # de-dupe, keep order; joined with ' & ' like bitworld bylines.
        seen = set()
        return " & ".join(n for n in names if not (n in seen or seen.add(n)))

    def group_string(prod_id):
        # Prefer explicit affiliation groups; otherwise author nicks that are
        # themselves groups.
        names = []
        for nid in affil_nicks.get(prod_id, []):
            nm = nick_name.get(nid)
            if nm:
                names.append(nm)
        if not names:
            for nid in author_nicks.get(prod_id, []):
                rel = nick_releaser.get(nid)
                if rel is not None and is_group.get(rel):
                    nm = nick_name.get(nid)
                    if nm:
                        names.append(nm)
        return join_names(names)

    def person_string(prod_id):
        # Author nicks belonging to a scener rather than a group.
        names = []
        for nid in author_nicks.get(prod_id, []):
            rel = nick_releaser.get(nid)
            if rel is not None and is_group.get(rel):
                continue
            nm = nick_name.get(nid)
            if nm:
                names.append(nm)
        return join_names(names)

    def author_string(prod_id, supertype):
        # A graphics entry is the work of the artist who drew it, so credit
        # them; demos and music keep the group byline.  An entry credited to
        # a group alone still falls back to that group.
        if supertype == "graphics" or supertype == "music":
            return person_string(prod_id) or group_string(prod_id)
        return group_string(prod_id)

    n = 0
    n_pouet = 0
    exported_platforms = {}  # platform -> lines written, for platform_stats()
    skipped_platform = 0
    skipped_download = 0
    skipped_tag = 0
    skipped_category = 0
    platformless = 0
    # Same reasoning as the db: write beside out_path and rename, so a run that
    # produces nothing (an empty or truncated db read back with --skip-load)
    # cannot replace a good export with an empty file for `just demozoo` to
    # gzip over the previous one.
    tmp_path = out_path + ".tmp"
    with open(tmp_path, "w", encoding="utf-8") as out:
        # Prose comments, not a `key:value` header: Demozoo is multi-platform,
        # so the platform is per line rather than set once for the whole file.
        out.write("# Demozoo release database (https://demozoo.org/)\n")
        out.write("# puae_model:date hatari_machinetype:date\n")
        for prod_id, title, date, precision, supertype in cur.execute(
            "SELECT id, title, release_date_date, release_date_precision, "
            "supertype FROM productions_production ORDER BY id"
        ):
            tags = sorted(
                tag_name[t] for t in prod_tags.get(prod_id, []) if tag_name.get(t)
            )
            if any(t in TAG_BLACKLIST for t in tags):
                skipped_tag += 1
                continue
            categories = [
                ptype_name[t] for t in prod_types.get(prod_id, []) if ptype_name.get(t)
            ]
            if any(c in CATEGORY_BLACKLIST for c in categories):
                skipped_category += 1
                continue
            raw_platforms = prod_platforms.get(prod_id, [])
            platforms = [platform_name[p] for p in raw_platforms if p in platform_name]
            if not platforms:
                # No platform *at all* is Demozoo not recording one (normal for
                # graphics and music); a platform we filtered out is a machine
                # we deliberately do not export, so that one still goes.  The
                # url check keeps out the platformless entries demarc could not
                # do anything with anyway.
                if (
                    raw_platforms
                    or supertype not in PLATFORMLESS_SUPERTYPES
                    or prod_id not in prod_urls
                ):
                    skipped_platform += 1
                    continue
                platformless += 1
            if prod_id in unusable:
                skipped_download += 1
                continue
            fields = [
                ("id", str(prod_id)),
                ("title", title or ""),
                ("author", author_string(prod_id, supertype)),
                ("date", fmt_date(date, precision)),
                ("party", prod_party.get(prod_id, "")),
                ("platform", ";".join(platforms)),
                ("category", ";".join(categories)),
                ("tags", ";".join(tags)),
                ("download", prod_urls.get(prod_id, "")),
                ("dlname", prod_dlnames.get(prod_id, "")),
            ]
            # Not every release has this field -- in toplist mode only the
            # few that made a list do -- so its absence, not an empty value,
            # means "no pouet numbers for this one".
            pd = pouet_lookup(prod_pouet_id.get(prod_id))
            if pd is not None:
                fields.append(("pouet", pd.field_value()))
                n_pouet += 1
            # A field with nothing after its colon carries no information, so
            # leave it out rather than write it empty.
            fields = [(key, clean(val)) for key, val in fields]
            out.write("\t".join(f"{key}:{val}" for key, val in fields if val) + "\n")
            n += 1
            for p in platforms:
                exported_platforms[p] = exported_platforms.get(p, 0) + 1

    if n == 0:
        os.remove(tmp_path)
        raise SystemExit(
            f"error: no releases to export -- {skipped_platform} productions "
            f"were off-whitelist, {skipped_download} had no usable download, "
            f"{skipped_tag} carried a blacklisted tag and {skipped_category} a "
            f"blacklisted production type.  If all counts are zero the "
            f"database is empty; rebuild it without --skip-load."
        )
    os.replace(tmp_path, out_path)

    platform_stats(cur, exported_platforms)
    print(
        f"Wrote {n} releases to {out_path} "
        f"({platformless} of them without a platform, "
        f"{n_pouet} with pouet data; skipped "
        f"{skipped_platform} off-whitelist platforms, "
        f"{skipped_download} without a usable download, "
        f"{skipped_tag} blacklisted tags, "
        f"{skipped_category} blacklisted categories)",
        file=sys.stderr,
    )


def main(argv=None):
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument(
        "--sql",
        default="demozoo-export.sql",
        help="Demozoo PostgreSQL dump (default: demozoo-export.sql)",
    )
    ap.add_argument(
        "--db",
        default="demozoo.sqlite",
        help="SQLite database to build/use (default: demozoo.sqlite)",
    )
    ap.add_argument(
        "--out", default="demozoo.txt", help="output file (default: demozoo.txt)"
    )
    ap.add_argument(
        "--skip-load",
        action="store_true",
        help="reuse an existing --db instead of rebuilding it",
    )
    ap.add_argument(
        "--pouet-cache",
        default=POUET_CACHE_DIR,
        help=f"where toplist pages are cached (default: {POUET_CACHE_DIR})",
    )
    ap.add_argument(
        "--pouet-prods",
        action="store_true",
        help="look each exported release up in pouet's data dump "
        "or, failing that, its prod API, instead of reading "
        "the toplists (covers every release with a pouet "
        "link, not just the toplisted ones)",
    )
    ap.add_argument(
        "--pouet-prod-cache",
        default=pouet.CACHE_DIR,
        help=f"where --pouet-prods caches API responses (default: {pouet.CACHE_DIR})",
    )
    ap.add_argument(
        "--pouet-limit",
        type=int,
        default=0,
        metavar="N",
        help="stop after N --pouet-prods fetches (0: no limit); "
        "cached pages do not count, so a trial run stays "
        "short and a later run picks up where it left off",
    )
    ap.add_argument(
        "--pouet-delay",
        type=float,
        default=POUET_PROD_DELAY,
        help=f"seconds between --pouet-prods fetches (default: {POUET_PROD_DELAY})",
    )
    ap.add_argument(
        "--refresh-pouet",
        action="store_true",
        help="refetch the pouet pages instead of using the cache",
    )
    ap.add_argument(
        "--no-pouet", action="store_true", help="skip pouet entirely (no pouet: field)"
    )
    args = ap.parse_args(argv)

    # One source or the other, never both: --pouet-prods covers every linked
    # release, the toplists only their top 64.
    pouet_data, pouet_lookup = {}, None
    if args.no_pouet:
        pass
    elif args.pouet_prods:
        pouet_lookup = pouet_prod_lookup(
            args.pouet_prod_cache,
            args.refresh_pouet,
            args.pouet_delay,
            args.pouet_limit,
        )
    else:
        pouet_data = load_pouetdata(args.pouet_cache, args.refresh_pouet)

    if args.skip_load:
        conn = sqlite3.connect(args.db)
    else:
        conn = load_database(args.sql, args.db)
    export(conn, args.out, pouet_data, pouet_lookup)
    conn.close()


if __name__ == "__main__":
    main()
