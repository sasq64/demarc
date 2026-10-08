"""populate_pouetdata against a saved Pouet toplist page (Amiga AGA, id 71),
so the test neither hits the network nor depends on today's vote counts."""

import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import demozoo

FIXTURE = Path(__file__).with_name("toplist_amiga_aga.html")


def test_populate_pouetdata(monkeypatch, tmp_path):
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(demozoo, "fetch_page", fake_fetch)
    rows = demozoo.populate_pouetdata(71, cache_dir=str(tmp_path))

    assert seen == ["https://www.pouet.net/toplist.php?type=&platform=71&limit=64"]
    assert len(rows) == 64
    # First row of the saved page: Starstruck by TBL, 576 thumbs, 18 CDCs.
    assert rows[0] == demozoo.PouetData(id=25778, thumbs=576, cncd_count=18)
    # Last row has no cdcstack div at all, which must read as zero rather than
    # throw or drop the release.
    assert rows[-1] == demozoo.PouetData(id=58665, thumbs=101, cncd_count=0)
    assert all(r.id > 0 and r.thumbs > 0 for r in rows)
    assert len({r.id for r in rows}) == 64

    # Second call is served from the disk cache, without refetching.
    assert demozoo.populate_pouetdata(71, cache_dir=str(tmp_path)) == rows
    assert len(seen) == 1
    # ...unless asked to refresh.
    demozoo.populate_pouetdata(71, cache_dir=str(tmp_path), refresh=True)
    assert len(seen) == 2


def make_db(prods, links, platform="Commodore 64"):
    """A minimal in-memory Demozoo db: one platform, `prods` productions and
    `links` (production_id, link_class, parameter, is_download_link) rows."""
    conn = sqlite3.connect(":memory:")
    cur = conn.cursor()
    for table, cols in demozoo.TABLES.items():
        cur.execute(f"CREATE TABLE {table} ({', '.join(cols)})")
    cur.execute("INSERT INTO platforms_platform VALUES (?, ?)", ("1", platform))
    for prod_id, title in prods:
        cur.execute(
            "INSERT INTO productions_production VALUES (?, ?, ?, ?, ?)",
            (prod_id, title, "1992-12-27", "d", "production"))
        cur.execute("INSERT INTO productions_production_platforms VALUES (?, ?)",
                    (prod_id, "1"))
    cur.executemany(
        "INSERT INTO productions_productionlink VALUES (?, ?, ?, ?)", links)
    conn.commit()
    return conn


def fields(line):
    return dict(f.split(":", 1) for f in line.rstrip("\n").split("\t"))


def test_export_pouet_field(tmp_path):
    conn = make_db(
        prods=[("1", "On the toplist"), ("2", "On pouet, not the toplist"),
               ("3", "Not on pouet")],
        links=[
            ("1", "BaseUrl", "http://example.com/a.zip", "t"),
            ("1", "PouetProduction", "25778", "f"),
            ("2", "BaseUrl", "http://example.com/b.zip", "t"),
            ("2", "PouetProduction", "999999", "f"),
            ("3", "BaseUrl", "http://example.com/c.zip", "t"),
        ])
    out = tmp_path / "demozoo.txt"
    demozoo.export(conn, str(out),
                   {25778: demozoo.PouetData(id=25778, thumbs=576, cncd_count=18)})

    lines = [l for l in out.read_text(encoding="utf-8").splitlines()
             if not l.startswith("#")]
    assert len(lines) == 3
    # A toplist row knows no rank and no awards, so those come out empty --
    # the field still has all five parts.
    assert fields(lines[0])["pouet"] == "18,576,0,,"
    assert "pouet" not in fields(lines[1])
    assert "pouet" not in fields(lines[2])
    # The pouet link is an external link and must not become a download.
    assert fields(lines[0])["download"] == "http://example.com/a.zip"


# Verified against the Rust `url` crate (the parser demarc actually uses) over
# every url of a full Demozoo export plus these cases; see url_parsable().
PARSABLE = [
    "http://example.com/a.zip",
    "http:example.com/a.zip",          # a special scheme gets its authority
    "file:///pub/foo.adf",             # ...and file: may go without a host
    "ftp://ftp.funet.fi/pub/x.lha",
    "mailto:a@b.com",                  # opaque path, nothing to fail
    "http://example.com:8080/x",
    "http://example.com:/x",           # empty port means the default one
    "http://[::1]:8080/x",
    "http://1.2.771/x",                # the last part absorbs the rest
    "http://ex%41mple.com/x",
    "http://example.com/a b.zip",      # only the *host* is held to the rules
    "https://ftp.modland.com/pub/modules/Protracker/## crimple ##.mod",
    "   http://example.com/x   ",      # trimmed before parsing
]

UNPARSABLE = [
    "fuckit",                          # the Demozoo BaseUrl parameters that
    "attach=29942",                    # are not urls at all
    "_-_minako.zip",
    ").zip",
    "     why me ?     ",
    "//example.com/x",                 # relative: no scheme to resolve with
    "/pub/foo.zip",
    "1http://example.com/x",           # a scheme starts with a letter
    "http://",                         # ...and a special scheme needs a host
    "http://example.com:99999/x",
    "http://example.com:80a/x",
    "http://[::1/x",                   # unclosed literal
    "http://[:::1]/x",
    "http://[fe80::1%25eth0]/x",       # no scope ids in the url standard
    "http://1.2.3.4.5/x",
    "http://999.1.1.1/x",
    "http://4294967296/x",
    "http://exa mple.com/x",
    "http://exa|mple.com/x",
    "http://ex%zzmple.com/x",
]


def test_url_parsable():
    for url in PARSABLE:
        assert demozoo.url_parsable(url), url
    for url in UNPARSABLE:
        assert not demozoo.url_parsable(url), url


def test_url_usable_rejects_semicolons():
    # Valid on its own, but the `download:` field separates its urls with ';',
    # so demarc would only ever see the halves.
    url = "https://www.cpcwiki.eu/forum/x/?action=dlattach;attach=31364"
    assert demozoo.url_parsable(url)
    assert not demozoo.url_usable(url)
    assert demozoo.url_usable("https://www.cpcwiki.eu/forum/x/?action=dlattach")


def test_export_skips_unparsable_downloads(tmp_path):
    conn = make_db(
        prods=[("1", "Good and bad urls"), ("2", "Only a bad url")],
        links=[
            ("1", "BaseUrl", "attach=29942", "t"),
            ("1", "BaseUrl", "http://x.com/a.php?action=dlattach;attach=31364", "t"),
            ("1", "BaseUrl", "http://example.com/a.zip", "t"),
            ("2", "BaseUrl", "fuckit", "t"),
        ])
    out = tmp_path / "demozoo.txt"
    demozoo.export(conn, str(out))

    lines = [l for l in out.read_text(encoding="utf-8").splitlines()
             if not l.startswith("#")]
    # A url demarc cannot parse is dropped, and a release left with none of them
    # goes the same way a wholly blacklisted one does.
    assert len(lines) == 1
    assert fields(lines[0])["title"] == "Good and bad urls"
    assert fields(lines[0])["download"] == "http://example.com/a.zip"


def test_export_skips_blacklisted_tags(tmp_path):
    conn = make_db(
        prods=[("1", "Runnable"), ("2", "Source only")],
        links=[("1", "BaseUrl", "http://example.com/a.zip", "t"),
               ("2", "BaseUrl", "http://example.com/b.zip", "t")])
    cur = conn.cursor()
    cur.execute("INSERT INTO taggit_tag VALUES (?, ?)", ("7", "no-binaries"))
    cur.execute("INSERT INTO taggit_tag VALUES (?, ?)", ("8", "fast"))
    cur.executemany("INSERT INTO taggit_taggeditem VALUES (?, ?, ?)", [
        ("8", "1", demozoo.PRODUCTION_CONTENT_TYPE),
        ("7", "2", demozoo.PRODUCTION_CONTENT_TYPE),
        # Same tag on a *different* model must not drop production 1.
        ("7", "1", "99"),
    ])
    conn.commit()
    out = tmp_path / "demozoo.txt"
    demozoo.export(conn, str(out))

    lines = [l for l in out.read_text(encoding="utf-8").splitlines()
             if not l.startswith("#")]
    assert len(lines) == 1
    assert fields(lines[0])["title"] == "Runnable"
    assert fields(lines[0])["tags"] == "fast"
