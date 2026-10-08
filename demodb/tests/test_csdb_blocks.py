"""The id-block walk in csdb.py, with the API replaced by a fake CSDb that has
releases up to a given id, so the tests do not hit the network."""

import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import csdb


@pytest.fixture
def fake_csdb(monkeypatch):
    """A CSDb holding ids 1..state['last']; state['calls'] logs the fetches."""
    state = {"last": 2500, "calls": [], "down": False}

    def fetch_block(min_id, max_id, release_type, retries=3):
        if state["down"]:
            raise OSError("no network")
        state["calls"].append((min_id, max_id))
        return [{"id": i} for i in range(min_id, min(max_id, state["last"]) + 1)]

    monkeypatch.setattr(csdb, "fetch_block", fetch_block)
    monkeypatch.setattr(csdb.time, "sleep", lambda s: None)
    return state


def test_walks_until_no_more_ids(fake_csdb, tmp_path):
    releases = csdb.load_releases(tmp_path, 1, None, 1)

    assert sorted(releases) == list(range(1, 2501))
    # Three blocks with releases and the empty one that ends the walk.
    assert fake_csdb["calls"] == [(1, 1001), (1001, 2001), (2001, 3001), (3001, 4001)]
    # The empty probe is not cached.
    assert not csdb.block_path(tmp_path, 3001, 4001).exists()


def test_rerun_refetches_only_the_tail(fake_csdb, tmp_path):
    csdb.load_releases(tmp_path, 1, None, 1)
    fake_csdb["calls"].clear()
    fake_csdb["last"] = 3200  # new releases since the last run

    releases = csdb.load_releases(tmp_path, 1, None, 1)

    assert sorted(releases) == list(range(1, 3201))
    # 1-1001 and 1001-2001 each have a cached successor, so only the last
    # cached block and what lies beyond it are asked for.
    assert fake_csdb["calls"] == [(2001, 3001), (3001, 4001), (4001, 5001)]


def test_offline_falls_back_to_cache(fake_csdb, tmp_path):
    csdb.load_releases(tmp_path, 1, None, 1)
    fake_csdb["down"] = True

    releases = csdb.load_releases(tmp_path, 1, None, 1)

    assert sorted(releases) == list(range(1, 2501))


def test_offline_without_cache_fails(fake_csdb, tmp_path):
    fake_csdb["down"] = True
    with pytest.raises(OSError):
        csdb.load_releases(tmp_path, 1, None, 1)


def test_fixed_max_stops_there(fake_csdb, tmp_path):
    releases = csdb.load_releases(tmp_path, 1, 1500, 1)

    assert sorted(releases) == list(range(1, 1501))
    assert fake_csdb["calls"] == [(1, 1001), (1001, 1500)]
