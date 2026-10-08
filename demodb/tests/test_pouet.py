"""pouet.get_prod against a saved API response (Megademica 4K, id 81065), so
the test neither hits the network nor depends on today's vote counts."""

import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import demozoo
import pouet

FIXTURE = Path(__file__).with_name("prod_81065.json")


def test_get_prod(monkeypatch, tmp_path):
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(pouet, "fetch_page", fake_fetch)
    prod = pouet.get_prod(81065, cache_dir=str(tmp_path))

    assert seen == ["https://api.pouet.net/v1/prod/?id=81065"]
    assert (prod.id, prod.name, prod.cdc) == (81065, "Megademica 4K", 1)
    assert (prod.rulez, prod.isok, prod.sucks) == (109, 3, 0)
    # The site shows '73%' for this one: popularity is the unrounded number.
    assert prod.popularity_pct == 73 and 73.0 <= prod.popularity < 74.0
    assert prod.rank == 767
    assert [str(a) for a in prod.awards] == ["nominee:40", "nominee:44"]

    # Second call is served from the disk cache, without refetching.
    assert pouet.get_prod(81065, cache_dir=str(tmp_path)) == prod
    assert len(seen) == 1
    # ...unless asked to refresh.
    pouet.get_prod(81065, cache_dir=str(tmp_path), refresh=True)
    assert len(seen) == 2


def test_vote_counts_are_strings_in_the_api():
    """The API spells vote counts as strings and cdc as a number; both have to
    come back as ints."""
    prod = pouet.parse_prod({"success": True, "prod": {
        "id": "981", "name": "Hardwired", "voteup": "292", "votepig": "16",
        "votedown": "0", "cdc": 24, "popularity": 84.6, "rank": "47"}})
    assert (prod.id, prod.cdc, prod.rulez, prod.isok, prod.sucks) == (
        981, 24, 292, 16, 0)
    assert prod.popularity_pct == 84
    assert prod.awards == []


def test_awards_split_by_type():
    """The `pouet:` field lists the won categories apart from the merely
    nominated ones, each in API order (VARIFORM, id 7138)."""
    prod = pouet.parse_prod({"success": True, "prod": {
        "id": "7138", "name": "VARIFORM", "voteup": "398", "cdc": 9,
        "rank": "19", "awards": [
            {"categoryID": "1", "awardType": "winner"},
            {"categoryID": "5", "awardType": "winner"},
            {"categoryID": "7", "awardType": "winner"},
            {"categoryID": "8", "awardType": "nominee"},
            {"categoryID": "55", "awardType": "nominee"}]}})
    assert prod.winner_ids == [1, 5, 7]
    assert prod.nominee_ids == [8, 55]

    pd = demozoo.PouetData(id=prod.id, thumbs=prod.rulez, cncd_count=prod.cdc,
                           rank=prod.rank, winners=prod.winner_ids,
                           nominees=prod.nominee_ids)
    assert pd.field_value() == "9,398,19,1 5 7,8 55"


def test_no_such_prod():
    """pouet answers 200 with {"error": true} for a prod that is not there."""
    assert pouet.parse_prod('{"error": true}', 424242424) is None
    assert pouet.parse_prod('not json at all', 1) is None
    assert pouet.parse_prod('{"success": true}', 1) is None


def test_error_responses_are_cached(monkeypatch, tmp_path):
    """Asking again would not make the prod exist, so the miss is cached too."""
    seen = []
    monkeypatch.setattr(pouet, "fetch_page",
                        lambda url: seen.append(url) or '{"error": true}')
    assert pouet.get_prod(424242424, cache_dir=str(tmp_path)) is None
    assert pouet.get_prod(424242424, cache_dir=str(tmp_path)) is None
    assert len(seen) == 1


def test_demozoo_prod_lookup(monkeypatch, tmp_path):
    """demozoo --pouet-prods maps an API response onto the same PouetData the
    toplists produce, and asks pouet for each prod only once."""
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(pouet, "fetch_page", fake_fetch)
    lookup = demozoo.pouet_prod_lookup(str(tmp_path), delay=0)

    assert lookup(81065) == demozoo.PouetData(id=81065, thumbs=109, cncd_count=1,
                                              rank=767, nominees=[40, 44])
    assert lookup(81065).cncd_count == 1   # served from the cache
    # Rank and awards ride along into the field the export writes.
    assert lookup(81065).field_value() == "1,109,767,,40 44"
    assert len(seen) == 1
    # A release with no pouet link asks nothing.
    assert lookup(None) is None
    assert len(seen) == 1


def test_demozoo_prod_lookup_skips_dead_prods(monkeypatch, tmp_path):
    monkeypatch.setattr(pouet, "fetch_page", lambda url: '{"error": true}')
    assert demozoo.pouet_prod_lookup(str(tmp_path), delay=0)(1) is None

    def boom(url):
        raise OSError("connection reset")

    monkeypatch.setattr(pouet, "fetch_page", boom)
    assert demozoo.pouet_prod_lookup(str(tmp_path), delay=0)(2) is None


def test_demozoo_prod_lookup_limit(monkeypatch, tmp_path):
    """--pouet-limit caps the fetches, not the export: past the limit the
    releases just come out without a pouet field.  Cached prods are free."""
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(pouet, "fetch_page", fake_fetch)
    lookup = demozoo.pouet_prod_lookup(str(tmp_path), delay=0, limit=2)

    assert lookup(1) is not None
    assert lookup(2) is not None
    assert lookup(3) is None          # over the limit: not even asked for
    assert len(seen) == 2
    assert lookup(1) is not None      # ...but the cache still answers
    assert len(seen) == 2


# --- the offline data dump -------------------------------------------------

# Two prods in pouet's dump format: a header line, one prod per line, and the
# last one without the trailing comma.  The rows are the API's rows, extra
# fields and all, so this is a cut-down copy of the real thing.
DUMP = (
    '{"dump_date":"2026-08-19 04:30:01","prods":[\n'
    '{"id":"981","name":"Hardwired","voteup":"292","votepig":"16",'
    '"votedown":"0","rank":"47","cdc":24,"popularity":84.96817521537272,'
    '"awards":[]},\n'
    '{"id":"81065","name":"Megademica 4K","voteup":"109","votepig":"3",'
    '"votedown":"0","rank":"771","cdc":1,"popularity":73.91163065499653,'
    '"awards":[{"id":"959","prodID":"81065","categoryID":"40",'
    '"awardType":"nominee"}]}\n'
    ']}\n'
)


@pytest.fixture
def dump(tmp_path, monkeypatch):
    """A dump file that pouet.py will find, via $POUET_DUMP."""
    path = tmp_path / "pouetdatadump-prods-20260819.json"
    path.write_text(DUMP, encoding="utf-8")
    monkeypatch.setenv(pouet.DUMP_ENV, str(path))
    return path


def test_dump_answers_without_the_network(dump, tmp_path, monkeypatch):
    """A prod in the dump costs no request, and parses to what the API would
    have given us."""
    monkeypatch.setattr(pouet, "fetch_page", _no_network)

    prod = pouet.get_prod(81065, cache_dir=str(tmp_path))
    assert (prod.id, prod.name, prod.cdc) == (81065, "Megademica 4K", 1)
    assert (prod.rulez, prod.isok, prod.sucks) == (109, 3, 0)
    assert prod.popularity_pct == 73 and prod.rank == 771
    assert [str(a) for a in prod.awards] == ["nominee:40"]

    # The last prod in the file has no trailing comma; it reads the same.
    assert pouet.get_prod(981, cache_dir=str(tmp_path)).rulez == 292
    # Nothing was cached, because nothing was fetched.
    assert not os.path.exists(pouet.cache_path(81065, str(tmp_path)))
    # And a dump prod counts as cached: callers pacing a run must not sleep.
    assert pouet.is_cached(81065, str(tmp_path))


def test_dump_is_read_once(dump, monkeypatch):
    """Indexing 160MB is cheap but not free, so it happens on the first lookup
    and not again."""
    reads = []
    real_open = open
    monkeypatch.setattr("builtins.open",
                        lambda f, *a, **k: reads.append(f) or real_open(f, *a, **k))
    pouet.dump_prod(981)
    pouet.dump_prod(81065)
    # ...naming the same dump explicitly is still the same dump.
    assert pouet.dump_prod(981, str(dump)).rulez == 292
    assert reads.count(str(dump)) == 1


def test_prods_missing_from_the_dump_go_to_the_api(dump, tmp_path, monkeypatch):
    """The dump is a snapshot: anything added after its date is still fetched
    (and cached) the old way."""
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(pouet, "fetch_page", fake_fetch)
    assert not pouet.in_dump(999999)
    assert pouet.get_prod(999999, cache_dir=str(tmp_path)).name == "Megademica 4K"
    assert seen == ["https://api.pouet.net/v1/prod/?id=999999"]
    assert pouet.is_cached(999999, str(tmp_path))    # cached, as before


def test_refresh_skips_the_dump(dump, tmp_path, monkeypatch):
    """--refresh is how you ask for today's votes, so it goes past the
    snapshot as well as past the cache."""
    seen = []

    def fake_fetch(url):
        seen.append(url)
        return FIXTURE.read_text(encoding="utf-8")

    monkeypatch.setattr(pouet, "fetch_page", fake_fetch)
    assert pouet.get_prod(81065, cache_dir=str(tmp_path), refresh=True) is not None
    assert seen == ["https://api.pouet.net/v1/prod/?id=81065"]


def test_no_dump_and_unreadable_dump_fall_back(tmp_path, monkeypatch, capsys):
    """A dump that is not there, or that pouet has reformatted past
    recognition, is not an error -- the API takes over."""
    monkeypatch.setenv(pouet.DUMP_ENV, str(tmp_path / "nope.json"))
    assert pouet.load_dump() is None
    assert "cannot read pouet dump" in capsys.readouterr().err

    reformatted = tmp_path / "pretty.json"
    reformatted.write_text('{\n  "prods": [\n    {\n      "id": "981"\n    }\n  ]\n}')
    monkeypatch.setenv(pouet.DUMP_ENV, str(reformatted))
    pouet._dump_loaded = False
    assert pouet.load_dump() is None
    assert "no prods found" in capsys.readouterr().err
    assert pouet.dump_prod(981) is None


def test_demozoo_prod_lookup_uses_the_dump(dump, tmp_path, monkeypatch):
    """The point of all this: an export of a hundred thousand prods asks
    pouet.net for none of them."""
    monkeypatch.setattr(pouet, "fetch_page", _no_network)
    lookup = demozoo.pouet_prod_lookup(str(tmp_path), delay=0, limit=1)

    assert lookup(81065) == demozoo.PouetData(id=81065, thumbs=109, cncd_count=1,
                                              rank=771, nominees=[40])
    # Dump prods are free, so --pouet-limit 1 does not stop at the first one.
    assert lookup(981) == demozoo.PouetData(id=981, thumbs=292, cncd_count=24,
                                            rank=47)


def _no_network(url):
    raise AssertionError(f"asked pouet.net for {url}")
