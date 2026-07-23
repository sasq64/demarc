#!/usr/bin/env python3
"""demozoo2bitworld — populate a SQLite database from a Demozoo PostgreSQL dump
and export all releases in the same tab-separated format as bitworld.txt.

bitworld columns:  ID  Title  Group  Date  Event  Type  Tags  URL

Usage:
    python demozoo2bitworld.py --sql demozoo-export.sql --db demozoo.sqlite \
        --out demozoo-bitworld.txt

By default the database is rebuilt from the SQL file. Pass --skip-load to reuse
an existing SQLite database and only regenerate the export (much faster).
"""

import argparse
import sqlite3
import sys
import time

# ---------------------------------------------------------------------------
# Which tables (and which of their columns) we pull out of the pg_dump.
# We only load what the export actually needs.
# ---------------------------------------------------------------------------
TABLES = {
    "productions_production": [
        "id", "title", "release_date_date", "release_date_precision", "supertype",
    ],
    "productions_production_author_nicks": ["production_id", "nick_id"],
    "productions_production_author_affiliation_nicks": ["production_id", "nick_id"],
    "demoscene_nick": ["id", "releaser_id", "name"],
    "demoscene_releaser": ["id", "is_group"],
    "productions_production_types": ["production_id", "productiontype_id"],
    "productions_productiontype": ["id", "name"],
    "parties_competitionplacing": ["competition_id", "production_id"],
    "parties_competition": ["id", "party_id"],
    "parties_party_releases": ["party_id", "production_id"],
    "parties_party": ["id", "name"],
    "productions_productionlink": [
        "production_id", "link_class", "parameter", "is_download_link",
    ],
    "taggit_tag": ["id", "name"],
    "taggit_taggeditem": ["tag_id", "object_id", "content_type_id"],
}

# django_content_type id of the 'production' model (productions app).
# Values load from the dump as TEXT, so keep this a string for comparison.
PRODUCTION_CONTENT_TYPE = "12"

# ---------------------------------------------------------------------------
# URL resolution.  A Demozoo productionlink stores a link_class plus a
# parameter; the real URL is reconstructed from a per-class template.  We
# implement the common ones.  `{p}` is the (stripped) parameter.
# ---------------------------------------------------------------------------
URL_TEMPLATES = {
    "BaseUrl": "{p}",
    "AmigascneFile": "http://ftp.amigascne.org/pub/amiga{p}",
    "SceneOrgFile": "https://files.scene.org/get{p}",
    "ModlandFile": "https://ftp.modland.com/pub/modules{p}",
    "FujiologyFile": "https://ftp.untergrund.net/users/ltk_tscl/fujiology{p}",
    "UntergrundFile": "https://ftp.untergrund.net{p}",
    "PaduaOrgFile": "http://ftp.padua.org/pub/c64{p}",
    "Defacto2File": "https://defacto2.net/f/{p}",
    "ModarchiveModule": "https://modarchive.org/module.php?{p}",
    "SixteenColorsPack": "https://16colo.rs/pack/{p}",
    "PouetProduction": "https://www.pouet.net/prod.php?which={p}",
    "CsdbRelease": "https://csdb.dk/release/?id={p}",
    "CsdbMusic": "https://csdb.dk/release/?id={p}",
    "ZxdemoItem": "https://zxdemo.org/item.php?id={p}",
    "KestraBitworldRelease": "http://janeway.exotica.org.uk/release.php?id={p}",
    "YoutubeVideo": "https://www.youtube.com/watch?v={p}",
    "VimeoVideo": "https://vimeo.com/{p}",
    "NectarineSong": "https://scenestream.net/demovibes/song/{p}/",
    "SoundcloudTrack": "https://soundcloud.com/{p}",
    "BandcampTrack": "https://{p}",
    "GithubRepo": "https://github.com/{p}",
    "InternetArchivePage": "https://archive.org/details/{p}",
    "SpotifyTrack": "https://open.spotify.com/track/{p}",
    "Tic80Cart": "https://tic80.com/play?cart={p}",
}

# Order in which link classes are preferred when choosing THE url for a row.
# Downloadable file archives first (matches bitworld, which points at files),
# then production pages, then media/streaming links.
URL_PRIORITY = [
    "AmigascneFile", "SceneOrgFile", "ModlandFile", "FujiologyFile",
    "UntergrundFile", "PaduaOrgFile", "Defacto2File", "ModarchiveModule",
    "SixteenColorsPack", "BaseUrl",
    "PouetProduction", "CsdbRelease", "CsdbMusic", "ZxdemoItem",
    "KestraBitworldRelease",
    "YoutubeVideo", "VimeoVideo", "NectarineSong", "SoundcloudTrack",
    "BandcampTrack", "GithubRepo", "InternetArchivePage", "SpotifyTrack",
    "Tic80Cart",
]
URL_RANK = {cls: i for i, cls in enumerate(URL_PRIORITY)}


def resolve_url(link_class, parameter):
    tmpl = URL_TEMPLATES.get(link_class)
    if tmpl is None:
        return None
    return tmpl.replace("{p}", parameter.strip())


# ---------------------------------------------------------------------------
# COPY-format parsing
# ---------------------------------------------------------------------------
_UNESCAPE = {
    "\\": "\\", "b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t", "v": "\v",
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
    conn = sqlite3.connect(db_path)
    conn.execute("PRAGMA journal_mode=OFF")
    conn.execute("PRAGMA synchronous=OFF")
    cur = conn.cursor()

    for table, cols in TABLES.items():
        cur.execute(f"DROP TABLE IF EXISTS {table}")
        cur.execute(f"CREATE TABLE {table} ({', '.join(cols)})")

    remaining = dict(TABLES)  # tables we still need to find in the dump
    current = None            # (table, [source col indices], cols)
    batch = []
    inserted = {t: 0 for t in TABLES}

    def flush():
        if current and batch:
            table = current[0]
            placeholders = ", ".join("?" * len(current[2]))
            cur.executemany(
                f"INSERT INTO {table} VALUES ({placeholders})", batch
            )
            inserted[table] += len(batch)
        batch.clear()

    t0 = time.time()
    with open(sql_path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if current is None:
                if line.startswith("COPY public.") and remaining:
                    # Is this one of the tables we care about?
                    name = line[len("COPY public."):line.index(" (")]
                    if name in remaining:
                        all_cols = line[line.index("(") + 1:line.index(")")].split(", ")
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
        print(f"  WARNING: tables not found in dump: {', '.join(remaining)}",
              file=sys.stderr)

    build_indexes(cur)
    conn.commit()
    return conn


def build_indexes(cur):
    for stmt in [
        "CREATE INDEX ix_pan_prod ON productions_production_author_nicks(production_id)",
        "CREATE INDEX ix_paan_prod ON productions_production_author_affiliation_nicks(production_id)",
        "CREATE INDEX ix_nick_id ON demoscene_nick(id)",
        "CREATE INDEX ix_rel_id ON demoscene_releaser(id)",
        "CREATE INDEX ix_ptypes_prod ON productions_production_types(production_id)",
        "CREATE INDEX ix_ptype_id ON productions_productiontype(id)",
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
    """Render a Demozoo (date, precision) pair as bitworld does."""
    if not date:
        return ""
    # date is ISO 'YYYY-MM-DD'
    if precision == "y":
        return date[:4]
    if precision == "m":
        return date[:7]
    return date  # 'd' or anything else -> full date


def scalar_maps(cur):
    """Pre-load small lookup tables into dicts for fast per-row assembly."""
    nick_name = {}
    nick_releaser = {}
    for nid, rel, name in cur.execute(
            "SELECT id, releaser_id, name FROM demoscene_nick"):
        nick_name[nid] = name
        nick_releaser[nid] = rel
    is_group = {}
    for rid, g in cur.execute("SELECT id, is_group FROM demoscene_releaser"):
        is_group[rid] = (g == "t")
    ptype_name = {}
    for pid, name in cur.execute("SELECT id, name FROM productions_productiontype"):
        ptype_name[pid] = name
    party_name = {}
    for pid, name in cur.execute("SELECT id, name FROM parties_party"):
        party_name[pid] = name
    tag_name = {}
    for tid, name in cur.execute("SELECT id, name FROM taggit_tag"):
        tag_name[tid] = name
    return nick_name, nick_releaser, is_group, ptype_name, party_name, tag_name


def group_multimap(cur, table, key_col, val_col):
    """Return {key: [val, ...]} preserving insertion order."""
    d = {}
    for k, v in cur.execute(f"SELECT {key_col}, {val_col} FROM {table}"):
        d.setdefault(k, []).append(v)
    return d


def export(conn, out_path):
    cur = conn.cursor()
    (nick_name, nick_releaser, is_group,
     ptype_name, party_name, tag_name) = scalar_maps(cur)

    author_nicks = group_multimap(
        cur, "productions_production_author_nicks", "production_id", "nick_id")
    affil_nicks = group_multimap(
        cur, "productions_production_author_affiliation_nicks",
        "production_id", "nick_id")
    prod_types = group_multimap(
        cur, "productions_production_types", "production_id", "productiontype_id")

    # production -> party name (via competition placing, else party_releases)
    comp_party = {}  # competition_id -> party_id
    for cid, pid in cur.execute("SELECT id, party_id FROM parties_competition"):
        comp_party[cid] = pid
    prod_party = {}
    for comp_id, prod_id in cur.execute(
            "SELECT competition_id, production_id FROM parties_competitionplacing"):
        party_id = comp_party.get(comp_id)
        if party_id is not None:
            prod_party.setdefault(prod_id, party_id)
    for party_id, prod_id in cur.execute(
            "SELECT party_id, production_id FROM parties_party_releases"):
        prod_party.setdefault(prod_id, party_id)

    # production -> tags
    prod_tags = {}
    for tag_id, obj_id, ct in cur.execute(
            "SELECT tag_id, object_id, content_type_id FROM taggit_taggeditem"):
        if ct == PRODUCTION_CONTENT_TYPE:
            prod_tags.setdefault(obj_id, []).append(tag_id)

    # production -> best url
    prod_url = {}
    prod_url_rank = {}
    for prod_id, link_class, parameter, is_dl in cur.execute(
            "SELECT production_id, link_class, parameter, is_download_link "
            "FROM productions_productionlink"):
        rank = URL_RANK.get(link_class)
        if rank is None:
            continue
        # Prefer download links: bias their rank ahead of non-download ones.
        if is_dl != "t":
            rank += 1000
        if prod_id not in prod_url_rank or rank < prod_url_rank[prod_id]:
            url = resolve_url(link_class, parameter)
            if url:
                prod_url[prod_id] = url
                prod_url_rank[prod_id] = rank

    def group_string(prod_id):
        # Prefer explicit affiliation groups; otherwise author nicks that are
        # themselves groups.  Joined with ' & ' like bitworld bylines.
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
        # de-dupe, keep order
        seen = set()
        uniq = [n for n in names if not (n in seen or seen.add(n))]
        return " & ".join(uniq)

    n = 0
    with open(out_path, "w", encoding="utf-8") as out:
        out.write("# Title Group Date Event Type Tags URL\n")
        for prod_id, title, date, precision, supertype in cur.execute(
                "SELECT id, title, release_date_date, release_date_precision, "
                "supertype FROM productions_production ORDER BY id"):
            group = group_string(prod_id)
            when = fmt_date(date, precision)
            event = party_name.get(prod_party.get(prod_id), "") or ""
            types = ",".join(
                ptype_name.get(t, "") for t in prod_types.get(prod_id, [])
                if ptype_name.get(t)
            )
            tags = ";".join(
                sorted(tag_name.get(t, "") for t in prod_tags.get(prod_id, [])
                       if tag_name.get(t))
            )
            url = prod_url.get(prod_id, "")

            row = [str(prod_id), title or "", group, when, event, types, tags, url]
            # Guard against stray tabs/newlines inside free-text fields.
            row = [c.replace("\t", " ").replace("\n", " ").replace("\r", " ")
                   for c in row]
            out.write("\t".join(row) + "\n")
            n += 1

    print(f"Wrote {n} releases to {out_path}", file=sys.stderr)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--sql", default="demozoo-export.sql",
                    help="Demozoo PostgreSQL dump (default: demozoo-export.sql)")
    ap.add_argument("--db", default="demozoo.sqlite",
                    help="SQLite database to build/use (default: demozoo.sqlite)")
    ap.add_argument("--out", default="demozoo-bitworld.txt",
                    help="output file (default: demozoo-bitworld.txt)")
    ap.add_argument("--skip-load", action="store_true",
                    help="reuse an existing --db instead of rebuilding it")
    args = ap.parse_args(argv)

    if args.skip_load:
        conn = sqlite3.connect(args.db)
    else:
        conn = load_database(args.sql, args.db)
    export(conn, args.out)
    conn.close()


if __name__ == "__main__":
    main()
