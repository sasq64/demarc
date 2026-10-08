"""The toplist scraping in csdb.py, against a saved CSDb toplist page (C64
Intro Collection, subtype 44 -- the shortest list there is), so the tests
neither hit the network nor depend on today's votes."""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import csdb

FIXTURE = Path(__file__).with_name("csdb_toplist_intro_collection.html")


def test_parse_toplist():
    ratings = csdb.parse_toplist(FIXTURE.read_text(encoding="utf-8"))

    # Six rated releases on the saved page, best first.
    assert len(ratings) == 6
    assert ratings[126570] == "10.00"
    assert ratings[122292] == "8.80"
    # The header row names no release and must not end up in the map.
    assert all(isinstance(rid, int) for rid in ratings)


def test_parse_toplist_without_table():
    assert csdb.parse_toplist("<html><body>nothing here</body></html>") == {}


def test_release_types(monkeypatch, tmp_path):
    seen = []

    def fake_fetch(cache_dir, type_id, refresh=False):
        seen.append(type_id)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(csdb, "toplist_html", fake_fetch)
    types = csdb.release_types(tmp_path)

    # One page names every other type through its `subtype` dropdown.
    assert seen == [csdb.SEED_TYPE]
    assert types[1] == "C64 Demo"
    assert types[2] == "C64 One-File Demo"
    assert types[44] == "C64 Intro Collection"
    assert len(types) > 20


def test_load_ratings_skips_unreachable_type(monkeypatch, tmp_path):
    def fake_fetch(cache_dir, type_id, refresh=False):
        if type_id != csdb.SEED_TYPE:
            raise OSError("no such page")
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(csdb, "toplist_html", fake_fetch)
    # Every type but the seed fails; the export keeps the ratings it did get.
    assert csdb.load_ratings(tmp_path) == csdb.parse_toplist(
        FIXTURE.read_text(encoding="utf-8")
    )


def test_to_row_rating():
    release = {"id": 1, "title": "Embryo", "type": "C64 Demo"}
    assert "\trating:7.31\t" in csdb.to_row(release, ["http://x/y.zip"], "7.31")
    # No rating is no field at all, as with every other empty value.
    assert "rating:" not in csdb.to_row(release, ["http://x/y.zip"])
